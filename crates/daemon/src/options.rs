use clap::Parser;

#[derive(Parser)]
#[command(name = "clipper-daemon")]
pub struct Options {
    #[arg(long, default_value = "http://127.0.0.1:8787")]
    pub server_url: String,
    #[arg(long, env = "CLIPPER_DISABLE_CLIPBOARD_WATCHING", value_parser = clap::builder::BoolishValueParser::new())]
    pub disable_clipboard_watching: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_watching_can_be_disabled_without_changing_the_server() {
        let options = Options::try_parse_from([
            "clipper-daemon",
            "--disable-clipboard-watching",
            "--server-url",
            "https://test.example",
        ])
        .unwrap();
        assert!(options.disable_clipboard_watching);
        assert_eq!(options.server_url, "https://test.example");
    }

    #[test]
    fn clipboard_watching_can_be_disabled_by_environment() {
        if let Ok(expected) = std::env::var("CLIPPER_OPTIONS_TEST_CHILD") {
            let options = Options::try_parse_from(["clipper-daemon"]);
            if expected == "invalid" {
                assert!(options.is_err());
            } else {
                assert_eq!(
                    options.unwrap().disable_clipboard_watching,
                    expected == "true"
                );
            }
            return;
        }
        for (value, expected) in [
            ("1", "true"),
            ("true", "true"),
            ("TRUE", "true"),
            ("TrUe", "true"),
            ("yes", "true"),
            ("YeS", "true"),
            ("0", "false"),
            ("false", "false"),
            ("FALSE", "false"),
            ("FaLsE", "false"),
            ("no", "false"),
            ("No", "false"),
            ("maybe", "invalid"),
        ] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "options::tests::clipboard_watching_can_be_disabled_by_environment",
                    "--exact",
                ])
                .env("CLIPPER_OPTIONS_TEST_CHILD", expected)
                .env("CLIPPER_DISABLE_CLIPBOARD_WATCHING", value)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "environment value {value}");
        }
    }
}
