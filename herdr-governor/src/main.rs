fn main() -> std::process::ExitCode {
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::main;
    use std::process::ExitCode;

    #[test]
    fn main_returns_success() {
        assert_eq!(
            main(),
            ExitCode::SUCCESS,
            "main must return ExitCode::SUCCESS"
        );
    }
}
