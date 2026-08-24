use leddy_interfaces::{FaultResetAuthorization, SafetyFaultCode, SafetyFaultRecord, SafetyLimits};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SafetyObservation {
    pub observed_at_unix_ms: u64,
    pub last_authenticated_command_unix_ms: u64,
    pub supply_millivolts: Option<u32>,
    pub current_milliamps: Option<u32>,
    pub temperature_millicelsius: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyDecision {
    pub brightness_limit: u8,
    pub fault_latched: bool,
    pub active_faults: Vec<SafetyFaultCode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultResetError {
    InvalidAuthorization,
    ReusedAuthorization,
    UnsafeConditions,
}

impl fmt::Display for FaultResetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidAuthorization => "fault reset authorization is invalid",
            Self::ReusedAuthorization => "fault reset authorization was already used",
            Self::UnsafeConditions => "fault reset is blocked while unsafe conditions remain",
        })
    }
}

impl std::error::Error for FaultResetError {}

#[derive(Debug, Clone)]
pub struct SafetyController {
    limits: SafetyLimits,
    fault_latched: bool,
    active_faults: Vec<SafetyFaultCode>,
    history: Vec<SafetyFaultRecord>,
    used_reset_authorizations: Vec<String>,
    last_telemetry_unix_ms: Option<u64>,
}

impl SafetyController {
    pub fn new(limits: SafetyLimits) -> Result<Self, leddy_interfaces::ValidationError> {
        limits.validate()?;
        Ok(Self {
            limits,
            fault_latched: false,
            active_faults: Vec::new(),
            history: Vec::new(),
            used_reset_authorizations: Vec::new(),
            last_telemetry_unix_ms: None,
        })
    }

    pub fn observe(&mut self, observation: SafetyObservation) -> SafetyDecision {
        let next_faults = self.detect_faults(observation);
        self.reconcile_history(&next_faults, observation.observed_at_unix_ms);
        self.active_faults = next_faults;
        if !self.active_faults.is_empty() {
            self.fault_latched = true;
        }
        self.decision()
    }

    pub fn reset_authorized(
        &mut self,
        authorization: &FaultResetAuthorization,
        now_unix_ms: u64,
    ) -> Result<SafetyDecision, FaultResetError> {
        authorization
            .validate_at(now_unix_ms)
            .map_err(|_| FaultResetError::InvalidAuthorization)?;
        if self
            .used_reset_authorizations
            .iter()
            .any(|used| used == &authorization.authorization_id)
        {
            return Err(FaultResetError::ReusedAuthorization);
        }
        if !self.active_faults.is_empty() {
            return Err(FaultResetError::UnsafeConditions);
        }

        self.fault_latched = false;
        self.used_reset_authorizations
            .push(authorization.authorization_id.clone());
        if self.used_reset_authorizations.len() > 32 {
            self.used_reset_authorizations.remove(0);
        }
        Ok(self.decision())
    }

    pub fn take_telemetry_slot(&mut self, now_unix_ms: u64) -> bool {
        let due = self.last_telemetry_unix_ms.is_none_or(|last| {
            now_unix_ms.saturating_sub(last) >= u64::from(self.limits.telemetry_interval_ms)
        });
        if due {
            self.last_telemetry_unix_ms = Some(now_unix_ms);
        }
        due
    }

    pub fn recent_faults(&self) -> &[SafetyFaultRecord] {
        &self.history
    }

    pub fn decision(&self) -> SafetyDecision {
        SafetyDecision {
            brightness_limit: if self.fault_latched {
                self.limits.fail_safe_brightness
            } else {
                u8::MAX
            },
            fault_latched: self.fault_latched,
            active_faults: self.active_faults.clone(),
        }
    }

    fn detect_faults(&self, observation: SafetyObservation) -> Vec<SafetyFaultCode> {
        let mut faults = Vec::new();
        let required_sensor_missing = self.limits.minimum_supply_millivolts.is_some()
            && observation.supply_millivolts.is_none()
            || self.limits.maximum_current_milliamps.is_some()
                && observation.current_milliamps.is_none()
            || self.limits.maximum_temperature_millicelsius.is_some()
                && observation.temperature_millicelsius.is_none();

        if required_sensor_missing {
            faults.push(SafetyFaultCode::SensorFailure);
        }
        if self
            .limits
            .minimum_supply_millivolts
            .zip(observation.supply_millivolts)
            .is_some_and(|(minimum, observed)| observed < minimum)
        {
            faults.push(SafetyFaultCode::UnderVoltage);
        }
        if self
            .limits
            .maximum_current_milliamps
            .zip(observation.current_milliamps)
            .is_some_and(|(maximum, observed)| observed > maximum)
        {
            faults.push(SafetyFaultCode::OverCurrent);
        }
        if self
            .limits
            .maximum_temperature_millicelsius
            .zip(observation.temperature_millicelsius)
            .is_some_and(|(maximum, observed)| observed > maximum)
        {
            faults.push(SafetyFaultCode::OverTemperature);
        }
        if observation
            .observed_at_unix_ms
            .saturating_sub(observation.last_authenticated_command_unix_ms)
            > u64::from(self.limits.communication_timeout_ms)
        {
            faults.push(SafetyFaultCode::CommunicationLoss);
        }
        faults
    }

    fn reconcile_history(&mut self, next_faults: &[SafetyFaultCode], now_unix_ms: u64) {
        for code in &self.active_faults {
            if !next_faults.contains(code)
                && let Some(record) = self
                    .history
                    .iter_mut()
                    .rev()
                    .find(|record| record.code == *code && record.cleared_at_unix_ms.is_none())
            {
                record.cleared_at_unix_ms = Some(now_unix_ms);
            }
        }
        for code in next_faults {
            if !self.active_faults.contains(code) {
                self.history.push(SafetyFaultRecord {
                    code: *code,
                    tripped_at_unix_ms: now_unix_ms,
                    cleared_at_unix_ms: None,
                });
            }
        }
        let capacity = usize::from(self.limits.fault_history_capacity);
        if self.history.len() > capacity {
            self.history.drain(..self.history.len() - capacity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> SafetyLimits {
        SafetyLimits {
            minimum_supply_millivolts: Some(4_500),
            maximum_current_milliamps: Some(2_000),
            maximum_temperature_millicelsius: Some(75_000),
            fail_safe_brightness: 0,
            communication_timeout_ms: 5_000,
            telemetry_interval_ms: 1_000,
            fault_history_capacity: 3,
        }
    }

    fn healthy(at: u64) -> SafetyObservation {
        SafetyObservation {
            observed_at_unix_ms: at,
            last_authenticated_command_unix_ms: at,
            supply_millivolts: Some(5_000),
            current_milliamps: Some(1_000),
            temperature_millicelsius: Some(40_000),
        }
    }

    #[test]
    fn threshold_crossing_latches_fail_safe_output_locally() {
        let mut controller = SafetyController::new(limits()).expect("valid limits");
        let mut over_current = healthy(1_000);
        over_current.current_milliamps = Some(2_001);

        let decision = controller.observe(over_current);

        assert_eq!(decision.brightness_limit, 0);
        assert!(decision.fault_latched);
        assert_eq!(decision.active_faults, vec![SafetyFaultCode::OverCurrent]);
    }

    #[test]
    fn communication_loss_never_restores_unlimited_output() {
        let mut controller = SafetyController::new(limits()).expect("valid limits");
        let mut stale = healthy(6_001);
        stale.last_authenticated_command_unix_ms = 1_000;

        controller.observe(stale);
        let recovered = controller.observe(healthy(7_000));

        assert!(recovered.active_faults.is_empty());
        assert!(recovered.fault_latched);
        assert_eq!(recovered.brightness_limit, 0);
    }

    #[test]
    fn reset_requires_safe_conditions_fresh_authorization_and_single_use() {
        let mut controller = SafetyController::new(limits()).expect("valid limits");
        let mut too_hot = healthy(1_000);
        too_hot.temperature_millicelsius = Some(90_000);
        controller.observe(too_hot);
        let authorization = FaultResetAuthorization {
            authorization_id: "reset-1".into(),
            issued_at_unix_ms: 900,
            expires_at_unix_ms: 2_000,
        };

        assert_eq!(
            controller.reset_authorized(&authorization, 1_000),
            Err(FaultResetError::UnsafeConditions)
        );
        controller.observe(healthy(1_100));
        assert!(
            !controller
                .reset_authorized(&authorization, 1_100)
                .expect("safe authorized reset")
                .fault_latched
        );
        assert_eq!(
            controller.reset_authorized(&authorization, 1_200),
            Err(FaultResetError::ReusedAuthorization)
        );
    }

    #[test]
    fn fault_history_and_telemetry_are_bounded() {
        let mut controller = SafetyController::new(limits()).expect("valid limits");
        assert!(controller.take_telemetry_slot(1_000));
        assert!(!controller.take_telemetry_slot(1_999));
        assert!(controller.take_telemetry_slot(2_000));

        for index in 0..5 {
            let mut fault = healthy(3_000 + index * 2);
            fault.current_milliamps = Some(2_001);
            controller.observe(fault);
            controller.observe(healthy(3_001 + index * 2));
        }

        assert_eq!(controller.recent_faults().len(), 3);
        assert!(
            controller
                .recent_faults()
                .iter()
                .all(|record| record.cleared_at_unix_ms.is_some())
        );
    }
}
