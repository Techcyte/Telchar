//! Disposable execution-authenticated WSS receiver fixture.

#[cfg(test)]
struct Authentication {
    execution: String,
    instance: String,
    credential: String,
    consumed: bool,
}

#[cfg(test)]
impl Authentication {
    fn new(execution: &str, instance: &str, credential: &str) -> Self {
        Self {
            execution: execution.to_owned(),
            instance: instance.to_owned(),
            credential: format!("Bearer {credential}"),
            consumed: false,
        }
    }

    fn accept(&mut self, request: &tungstenite::http::Request<()>) -> bool {
        let matches = |name: &str, expected: &str| {
            request.headers().get(name).is_some_and(|value| value.as_bytes() == expected.as_bytes())
        };
        if self.consumed || request.uri().path() != "/fixture"
            || !matches("x-telchar-execution", &self.execution)
            || !matches("x-telchar-microvm", &self.instance)
            || !matches("authorization", &self.credential)
        {
            return false;
        }
        self.consumed = true;
        true
    }
}

fn main() {}

#[cfg(test)]
mod tests {
    use super::*;
    use tungstenite::http::Request;

    #[test]
    fn authentication_binds_instance_execution_and_single_use_credential() {
        let mut authentication = Authentication::new("execution", "instance", "secret");
        let request = |execution: &str, instance: &str, credential: &str| {
            Request::builder()
                .uri("/fixture")
                .header("x-telchar-execution", execution)
                .header("x-telchar-microvm", instance)
                .header("authorization", format!("Bearer {credential}"))
                .body(())
                .unwrap()
        };
        for rejected in [
            request("foreign", "instance", "secret"),
            request("execution", "foreign", "secret"),
            request("execution", "instance", "wrong"),
        ] {
            assert!(!authentication.accept(&rejected));
        }
        let accepted = request("execution", "instance", "secret");
        assert!(authentication.accept(&accepted));
        assert!(!authentication.accept(&accepted));
    }
}
