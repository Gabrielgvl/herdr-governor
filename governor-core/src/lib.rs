#![no_std]

/// Herdr protocol revision this build of the governor speaks.
#[must_use]
pub fn herdr_protocol() -> u32 {
    22
}

#[cfg(test)]
mod tests {
    use super::herdr_protocol;

    #[test]
    fn herdr_protocol_is_22() {
        assert_eq!(herdr_protocol(), 22, "protocol revision must be 22");
    }
}
