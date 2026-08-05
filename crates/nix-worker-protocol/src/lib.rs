#![forbid(unsafe_code)]

pub fn protocol_name() -> &'static str {
    "Nix worker protocol"
}

#[cfg(test)]
mod tests {
    use super::protocol_name;

    #[test]
    fn reports_protocol_name() {
        assert_eq!(protocol_name(), "Nix worker protocol");
    }
}
