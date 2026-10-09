use super::error::SupervisorError;
use crate::model::SessionEnvironment;
use std::{env, fs, path::PathBuf};
impl SessionEnvironment {
    pub fn capture() -> Result<Self, SupervisorError> {
        let names = [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "LLXPRT_CONFIG_HOME",
        ];
        let values: std::collections::BTreeMap<_, _> = names
            .into_iter()
            .filter_map(|name| env::var_os(name).map(|value| (name.to_owned(), value)))
            .collect();
        let home = values
            .get("HOME")
            .map(PathBuf::from)
            .ok_or(SupervisorError::Conflict)?;
        Self::capture_from(home, &values)
    }

    fn capture_from(
        home: PathBuf,
        values: &std::collections::BTreeMap<String, std::ffi::OsString>,
    ) -> Result<Self, SupervisorError> {
        if !home.is_absolute() {
            return Err(SupervisorError::Conflict);
        }
        let home = fs::canonicalize(home)?;
        fn absolute_override(
            name: &str,
            values: &std::collections::BTreeMap<String, std::ffi::OsString>,
        ) -> Result<Option<PathBuf>, SupervisorError> {
            match values.get(name).map(PathBuf::from) {
                None => Ok(None),
                Some(path) if path.is_absolute() => Ok(Some(path)),
                Some(_) => Err(SupervisorError::Conflict),
            }
        }
        Ok(Self {
            home,
            xdg_config_home: absolute_override("XDG_CONFIG_HOME", values)?,
            xdg_data_home: absolute_override("XDG_DATA_HOME", values)?,
            xdg_state_home: absolute_override("XDG_STATE_HOME", values)?,
            llxprt_config_home: absolute_override("LLXPRT_CONFIG_HOME", values)?,
        })
    }

    fn matches(&self, current: &Self) -> bool {
        self == current
    }

    pub fn matches_current(&self) -> Result<bool, SupervisorError> {
        Ok(self.matches(&Self::capture()?))
    }
}

#[cfg(test)]
mod session_environment_tests {
    use super::{SessionEnvironment, SupervisorError};
    use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

    fn values(entries: &[(&str, &str)]) -> BTreeMap<String, OsString> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
            .collect()
    }

    fn capture(
        home: &str,
        entries: &[(&str, &str)],
    ) -> Result<SessionEnvironment, SupervisorError> {
        SessionEnvironment::capture_from(PathBuf::from(home), &values(entries))
    }

    fn temp_home(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("llxprt-session-environment-{name}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn capture_canonicalizes_home_and_compares_all_overrides() {
        let home = temp_home("same");
        let home_text = home.to_str().unwrap();
        let env = capture(home_text, &[("HOME", home_text)]).unwrap();
        let same = capture(home_text, &[("HOME", home_text)]).unwrap();
        assert_eq!(env, same);
        assert!(env.matches(&same));

        let other_home = temp_home("other");
        assert!(
            !env.matches(
                &capture(
                    other_home.to_str().unwrap(),
                    &[("HOME", other_home.to_str().unwrap())]
                )
                .unwrap()
            )
        );

        let xdg_a = capture(home_text, &[("XDG_CONFIG_HOME", "/tmp/config-a")]).unwrap();
        let xdg_b = capture(home_text, &[("XDG_CONFIG_HOME", "/tmp/config-b")]).unwrap();
        assert!(!xdg_a.matches(&xdg_b));

        let llxprt_a = capture(home_text, &[("LLXPRT_CONFIG_HOME", "/tmp/llxprt-a")]).unwrap();
        let llxprt_b = capture(home_text, &[("LLXPRT_CONFIG_HOME", "/tmp/llxprt-b")]).unwrap();
        assert!(!llxprt_a.matches(&llxprt_b));
    }

    #[test]
    fn capture_rejects_missing_home_and_relative_paths() {
        assert!(matches!(
            SessionEnvironment::capture_from(PathBuf::from("relative-home"), &BTreeMap::new()),
            Err(SupervisorError::Conflict)
        ));
        assert!(matches!(
            capture("relative-home", &[]),
            Err(SupervisorError::Conflict)
        ));

        let home = temp_home("relative-overrides");
        let home_text = home.to_str().unwrap();
        assert!(matches!(
            capture(home_text, &[("XDG_CONFIG_HOME", "relative-config")]),
            Err(SupervisorError::Conflict)
        ));
    }
}
