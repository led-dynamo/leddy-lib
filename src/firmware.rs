use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use leddy_interfaces::{
    FirmwareBootHealth, FirmwareImageIdentity, FirmwareUpdateTelemetry, SignedFirmwareManifest,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareUpdatePolicy {
    pub controller_family: String,
    pub hardware_revision: String,
    pub protocol_version: u16,
    pub minimum_rollback_epoch: u64,
    pub boot_attempt_limit: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedFirmwareKey {
    pub key_id: String,
    pub public_key_ed25519: [u8; 32],
    pub minimum_rollback_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareUpdateError {
    InvalidManifest,
    UnknownSigningKey,
    RevokedSigningKey,
    InvalidSigningKey,
    WrongControllerFamily,
    IncompatibleHardware,
    IncompatibleProtocol,
    RollbackForbidden,
    ArtifactSizeMismatch,
    ArtifactDigestMismatch,
    InvalidSignatureEncoding,
    InvalidSignature,
    NoPendingUpdate,
    InvalidBootAttemptLimit,
}

impl fmt::Display for FirmwareUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidManifest => "firmware manifest is invalid",
            Self::UnknownSigningKey => "firmware signing key is not trusted",
            Self::RevokedSigningKey => "firmware signing key is revoked",
            Self::InvalidSigningKey => "firmware signing key is malformed",
            Self::WrongControllerFamily => "firmware targets a different controller family",
            Self::IncompatibleHardware => "firmware does not support this hardware revision",
            Self::IncompatibleProtocol => "firmware does not support this protocol version",
            Self::RollbackForbidden => "firmware rollback epoch is below the accepted minimum",
            Self::ArtifactSizeMismatch => "firmware artifact size does not match its manifest",
            Self::ArtifactDigestMismatch => "firmware artifact digest does not match its manifest",
            Self::InvalidSignatureEncoding => "firmware signature encoding is invalid",
            Self::InvalidSignature => "firmware manifest signature is invalid",
            Self::NoPendingUpdate => "no verified firmware update is staged",
            Self::InvalidBootAttemptLimit => "boot attempt limit must be greater than zero",
        })
    }
}

impl std::error::Error for FirmwareUpdateError {}

#[derive(Debug, Clone)]
pub struct FirmwareUpdateController {
    policy: FirmwareUpdatePolicy,
    trusted_keys: BTreeMap<String, TrustedFirmwareKey>,
    revoked_key_ids: Vec<String>,
    active: FirmwareImageIdentity,
    previous: Option<FirmwareImageIdentity>,
    pending: Option<FirmwareImageIdentity>,
    boot_health: FirmwareBootHealth,
    boot_attempts: u8,
    rollback_reason: Option<String>,
}

impl FirmwareUpdateController {
    pub fn new(
        policy: FirmwareUpdatePolicy,
        active: FirmwareImageIdentity,
        trusted_keys: impl IntoIterator<Item = TrustedFirmwareKey>,
    ) -> Result<Self, FirmwareUpdateError> {
        if policy.boot_attempt_limit == 0 {
            return Err(FirmwareUpdateError::InvalidBootAttemptLimit);
        }
        let trusted_keys = trusted_keys
            .into_iter()
            .map(|key| (key.key_id.clone(), key))
            .collect();
        Ok(Self {
            policy,
            trusted_keys,
            revoked_key_ids: Vec::new(),
            active,
            previous: None,
            pending: None,
            boot_health: FirmwareBootHealth::Healthy,
            boot_attempts: 0,
            rollback_reason: None,
        })
    }

    pub fn trust_key(&mut self, key: TrustedFirmwareKey) {
        self.revoked_key_ids.retain(|key_id| key_id != &key.key_id);
        self.trusted_keys.insert(key.key_id.clone(), key);
    }

    pub fn revoke_key(&mut self, key_id: &str) {
        self.trusted_keys.remove(key_id);
        if !self.revoked_key_ids.iter().any(|revoked| revoked == key_id) {
            self.revoked_key_ids.push(key_id.to_owned());
            if self.revoked_key_ids.len() > 64 {
                self.revoked_key_ids.remove(0);
            }
        }
    }

    pub fn verify_and_stage(
        &mut self,
        signed: &SignedFirmwareManifest,
        artifact: &[u8],
    ) -> Result<FirmwareImageIdentity, FirmwareUpdateError> {
        let manifest = &signed.manifest;
        manifest
            .validate()
            .map_err(|_| FirmwareUpdateError::InvalidManifest)?;
        if manifest.controller_family != self.policy.controller_family {
            return Err(FirmwareUpdateError::WrongControllerFamily);
        }
        if !manifest.supports_hardware(&self.policy.hardware_revision) {
            return Err(FirmwareUpdateError::IncompatibleHardware);
        }
        if !manifest.supports_protocol(self.policy.protocol_version) {
            return Err(FirmwareUpdateError::IncompatibleProtocol);
        }
        if manifest.rollback_epoch < self.policy.minimum_rollback_epoch
            || manifest.rollback_epoch < self.active.rollback_epoch
        {
            return Err(FirmwareUpdateError::RollbackForbidden);
        }
        if artifact.len() as u64 != manifest.artifact_size_bytes {
            return Err(FirmwareUpdateError::ArtifactSizeMismatch);
        }
        let artifact_digest = format!("{:x}", Sha256::digest(artifact));
        if artifact_digest != manifest.artifact_sha256 {
            return Err(FirmwareUpdateError::ArtifactDigestMismatch);
        }
        if self
            .revoked_key_ids
            .iter()
            .any(|key_id| key_id == &manifest.signing_key_id)
        {
            return Err(FirmwareUpdateError::RevokedSigningKey);
        }
        let trusted_key = self
            .trusted_keys
            .get(&manifest.signing_key_id)
            .ok_or(FirmwareUpdateError::UnknownSigningKey)?;
        if manifest.rollback_epoch < trusted_key.minimum_rollback_epoch {
            return Err(FirmwareUpdateError::RollbackForbidden);
        }
        let verifying_key = VerifyingKey::from_bytes(&trusted_key.public_key_ed25519)
            .map_err(|_| FirmwareUpdateError::InvalidSigningKey)?;
        let signature_bytes = STANDARD
            .decode(&signed.signature_ed25519_base64)
            .map_err(|_| FirmwareUpdateError::InvalidSignatureEncoding)?;
        let signature = Signature::from_slice(&signature_bytes)
            .map_err(|_| FirmwareUpdateError::InvalidSignatureEncoding)?;
        let canonical_manifest =
            serde_json::to_vec(manifest).map_err(|_| FirmwareUpdateError::InvalidManifest)?;
        verifying_key
            .verify(&canonical_manifest, &signature)
            .map_err(|_| FirmwareUpdateError::InvalidSignature)?;

        let identity = FirmwareImageIdentity {
            firmware_version: manifest.firmware_version.clone(),
            artifact_sha256: manifest.artifact_sha256.clone(),
            signing_key_id: manifest.signing_key_id.clone(),
            rollback_epoch: manifest.rollback_epoch,
        };
        self.pending = Some(identity.clone());
        self.rollback_reason = None;
        Ok(identity)
    }

    pub fn activate_staged(&mut self) -> Result<(), FirmwareUpdateError> {
        let pending = self
            .pending
            .take()
            .ok_or(FirmwareUpdateError::NoPendingUpdate)?;
        self.previous = Some(std::mem::replace(&mut self.active, pending));
        self.boot_health = FirmwareBootHealth::Pending;
        self.boot_attempts = 0;
        self.rollback_reason = None;
        Ok(())
    }

    pub fn record_boot_result(&mut self, healthy: bool) {
        if healthy {
            self.boot_health = FirmwareBootHealth::Healthy;
            self.policy.minimum_rollback_epoch = self
                .policy
                .minimum_rollback_epoch
                .max(self.active.rollback_epoch);
            self.previous = None;
            self.boot_attempts = 0;
            self.rollback_reason = None;
            return;
        }

        self.boot_attempts = self.boot_attempts.saturating_add(1);
        self.boot_health = FirmwareBootHealth::Failed;
        if self.boot_attempts >= self.policy.boot_attempt_limit
            && let Some(previous) = self.previous.take()
        {
            self.active = previous;
            self.boot_health = FirmwareBootHealth::RolledBack;
            self.rollback_reason = Some("boot health check attempt limit exceeded".into());
            self.boot_attempts = 0;
        }
    }

    pub fn telemetry(&self) -> FirmwareUpdateTelemetry {
        FirmwareUpdateTelemetry {
            active: self.active.clone(),
            pending: self.pending.clone(),
            boot_health: self.boot_health,
            rollback_reason: self.rollback_reason.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use leddy_interfaces::FirmwareManifest;

    fn active() -> FirmwareImageIdentity {
        FirmwareImageIdentity {
            firmware_version: "1.0.0".into(),
            artifact_sha256: "00".repeat(32),
            signing_key_id: "release-1".into(),
            rollback_epoch: 3,
        }
    }

    fn controller(signing_key: &SigningKey) -> FirmwareUpdateController {
        FirmwareUpdateController::new(
            FirmwareUpdatePolicy {
                controller_family: "leddy-controller".into(),
                hardware_revision: "esp32-s3-r1".into(),
                protocol_version: 3,
                minimum_rollback_epoch: 3,
                boot_attempt_limit: 2,
            },
            active(),
            [TrustedFirmwareKey {
                key_id: "release-1".into(),
                public_key_ed25519: signing_key.verifying_key().to_bytes(),
                minimum_rollback_epoch: 3,
            }],
        )
        .expect("valid policy")
    }

    fn signed_manifest(signing_key: &SigningKey, artifact: &[u8]) -> SignedFirmwareManifest {
        let manifest = FirmwareManifest {
            schema_version: 1,
            controller_family: "leddy-controller".into(),
            compatible_hardware_revisions: vec!["esp32-s3-r1".into(), "stm32-f4-r2".into()],
            firmware_version: "2.0.0".into(),
            protocol_minimum: 2,
            protocol_maximum: 4,
            artifact_sha256: format!("{:x}", Sha256::digest(artifact)),
            artifact_size_bytes: artifact.len() as u64,
            source_commit: "0123456789abcdef".into(),
            reproducible_build_id: "nix:leddy-firmware-2.0.0".into(),
            rollback_epoch: 4,
            signing_key_id: "release-1".into(),
        };
        let signature = signing_key.sign(&serde_json::to_vec(&manifest).expect("serialize"));
        SignedFirmwareManifest {
            manifest,
            signature_ed25519_base64: STANDARD.encode(signature.to_bytes()),
        }
    }

    #[test]
    fn verifies_digest_signature_hardware_and_rollback_before_staging() {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let artifact = b"verified firmware image";
        let signed = signed_manifest(&signing_key, artifact);
        let mut controller = controller(&signing_key);

        let staged = controller
            .verify_and_stage(&signed, artifact)
            .expect("verified image should stage");

        assert_eq!(staged.firmware_version, "2.0.0");
        assert_eq!(controller.telemetry().pending, Some(staged));
        assert_eq!(controller.telemetry().active, active());
    }

    #[test]
    fn rejects_tampered_wrong_board_revoked_and_rollback_images() {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let artifact = b"verified firmware image";
        let mut signed = signed_manifest(&signing_key, artifact);
        let mut controller = controller(&signing_key);

        assert_eq!(
            controller.verify_and_stage(&signed, b"tampered firmware image"),
            Err(FirmwareUpdateError::ArtifactDigestMismatch)
        );
        signed.manifest.compatible_hardware_revisions = vec!["stm32-f4-r2".into()];
        assert_eq!(
            controller.verify_and_stage(&signed, artifact),
            Err(FirmwareUpdateError::IncompatibleHardware)
        );

        let mut signed = signed_manifest(&signing_key, artifact);
        signed.manifest.rollback_epoch = 2;
        assert_eq!(
            controller.verify_and_stage(&signed, artifact),
            Err(FirmwareUpdateError::RollbackForbidden)
        );

        let signed = signed_manifest(&signing_key, artifact);
        controller.revoke_key("release-1");
        assert_eq!(
            controller.verify_and_stage(&signed, artifact),
            Err(FirmwareUpdateError::RevokedSigningKey)
        );
    }

    #[test]
    fn activation_is_atomic_and_failed_health_checks_restore_last_good_image() {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let artifact = b"verified firmware image";
        let signed = signed_manifest(&signing_key, artifact);
        let mut controller = controller(&signing_key);

        controller
            .verify_and_stage(&signed, artifact)
            .expect("verified image should stage");
        assert_eq!(controller.telemetry().active, active());
        controller.activate_staged().expect("staged image");
        assert_eq!(controller.telemetry().active.firmware_version, "2.0.0");
        assert_eq!(
            controller.telemetry().boot_health,
            FirmwareBootHealth::Pending
        );

        controller.record_boot_result(false);
        assert_eq!(
            controller.telemetry().boot_health,
            FirmwareBootHealth::Failed
        );
        controller.record_boot_result(false);
        let telemetry = controller.telemetry();
        assert_eq!(telemetry.active, active());
        assert_eq!(telemetry.boot_health, FirmwareBootHealth::RolledBack);
        assert!(telemetry.rollback_reason.is_some());
    }
}
