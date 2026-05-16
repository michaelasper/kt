use crate::Language;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileRole {
    Implementation,
    Test,
    Fixture,
    Generated,
    Config,
}

impl FileRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implementation => "implementation",
            Self::Test => "test",
            Self::Fixture => "fixture",
            Self::Generated => "generated",
            Self::Config => "config",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "implementation" => Some(Self::Implementation),
            "test" => Some(Self::Test),
            "fixture" => Some(Self::Fixture),
            "generated" => Some(Self::Generated),
            "config" => Some(Self::Config),
            _ => None,
        }
    }

    pub fn detect(relative_path: &str, language: Language) -> Self {
        let path = relative_path.replace('\\', "/");
        let path_lower = path.to_ascii_lowercase();
        let filename = path.rsplit('/').next().unwrap_or(&path);

        if Self::is_test_path(&path_lower, filename, language) {
            return Self::Test;
        }
        if Self::is_generated_path(&path_lower, filename, language) {
            return Self::Generated;
        }
        if Self::is_fixture_path(&path_lower) {
            return Self::Fixture;
        }
        if Self::is_config_path(&path, filename, language) {
            return Self::Config;
        }
        Self::Implementation
    }

    fn is_test_path(path_lower: &str, filename: &str, language: Language) -> bool {
        if path_lower.contains("/test/")
            || path_lower.contains("/tests/")
            || path_lower.contains("/__tests__/")
            || path_lower.contains("/spec/")
        {
            return true;
        }
        match language {
            Language::Rust => filename.ends_with("_test.rs"),
            Language::Go => filename.ends_with("_test.go"),
            Language::Java => {
                filename.ends_with("test.java")
                    || filename.ends_with("tests.java")
                    || filename.ends_with("it.java")
                    || path_lower.contains("src/test/")
            }
            Language::Python => {
                filename.starts_with("test_")
                    || filename.ends_with("_test.py")
                    || filename.ends_with("_tests.py")
            }
            Language::Swift => filename.ends_with("tests.swift") || filename.contains("test"),
            Language::ObjectiveC => {
                filename.ends_with("test.m")
                    || filename.ends_with("tests.m")
                    || filename.ends_with("spec.m")
                    || filename.ends_with("specs.m")
            }
            Language::TypeScript | Language::Tsx | Language::Javascript => {
                filename.ends_with(".test.ts")
                    || filename.ends_with(".test.tsx")
                    || filename.ends_with(".test.js")
                    || filename.ends_with(".spec.ts")
                    || filename.ends_with(".spec.tsx")
                    || filename.ends_with(".spec.js")
            }
            Language::Markdown | Language::Html => false,
        }
    }

    fn is_generated_path(path_lower: &str, filename: &str, language: Language) -> bool {
        match language {
            Language::Go => {
                filename.ends_with(".pb.go")
                    || filename.ends_with(".generated.go")
                    || path_lower.contains(".pb.")
            }
            Language::Python => {
                filename.ends_with("_pb2.py")
                    || filename.ends_with("_pb2_grpc.py")
                    || filename.contains(".generated.")
            }
            Language::Java => filename.contains(".generated.") || filename.ends_with(".grpc.java"),
            _ => filename.contains(".generated."),
        }
    }

    fn is_fixture_path(path_lower: &str) -> bool {
        path_lower.contains("/fixtures/")
            || path_lower.contains("/__fixtures__/")
            || path_lower.contains("/testdata/")
            || path_lower.contains("/test_data/")
            || path_lower.contains("/testfixtures/")
    }

    fn is_config_path(path: &str, filename: &str, language: Language) -> bool {
        match language {
            Language::Rust => filename.ends_with(".config.rs") || path.contains("config/"),
            Language::Go => filename.ends_with("_config.go") || path.contains("config/"),
            Language::Java => path.contains("config/") || filename.ends_with("Config.java"),
            Language::Python => path.contains("config/") || filename.starts_with("config_"),
            _ => path.contains("config/"),
        }
    }
}

impl std::fmt::Display for FileRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_test_file() {
        assert_eq!(
            FileRole::detect("src/foo_test.rs", Language::Rust),
            FileRole::Test
        );
        assert_eq!(
            FileRole::detect("src/tests/integration.rs", Language::Rust),
            FileRole::Test
        );
    }

    #[test]
    fn test_java_test_file() {
        assert_eq!(
            FileRole::detect("src/test/java/com/example/UserTest.java", Language::Java),
            FileRole::Test
        );
        assert_eq!(
            FileRole::detect("src/main/java/com/example/User.java", Language::Java),
            FileRole::Implementation
        );
    }

    #[test]
    fn test_go_test_file() {
        assert_eq!(
            FileRole::detect("handler_test.go", Language::Go),
            FileRole::Test
        );
        assert_eq!(
            FileRole::detect("handler.go", Language::Go),
            FileRole::Implementation
        );
    }

    #[test]
    fn test_python_test_file() {
        assert_eq!(
            FileRole::detect("tests/test_auth.py", Language::Python),
            FileRole::Test
        );
        assert_eq!(
            FileRole::detect("auth/__tests__/test_login.py", Language::Python),
            FileRole::Test
        );
    }

    #[test]
    fn test_typescript_test_file() {
        assert_eq!(
            FileRole::detect("app/login.test.ts", Language::TypeScript),
            FileRole::Test
        );
        assert_eq!(
            FileRole::detect("app/login.spec.tsx", Language::Tsx),
            FileRole::Test
        );
    }

    #[test]
    fn test_generated_file() {
        assert_eq!(
            FileRole::detect("api.pb.go", Language::Go),
            FileRole::Generated
        );
        assert_eq!(
            FileRole::detect("foo_pb2.py", Language::Python),
            FileRole::Generated
        );
    }

    #[test]
    fn test_fixture_path() {
        assert_eq!(
            FileRole::detect("tests/fixtures/data.json", Language::Python),
            FileRole::Fixture
        );
    }

    #[test]
    fn test_implementation_default() {
        assert_eq!(
            FileRole::detect("src/main.rs", Language::Rust),
            FileRole::Implementation
        );
        assert_eq!(
            FileRole::detect("lib.rs", Language::Rust),
            FileRole::Implementation
        );
    }

    #[test]
    fn test_swift_test_file() {
        assert_eq!(
            FileRole::detect("MyApp/Tests/MyAppTests.swift", Language::Swift),
            FileRole::Test
        );
    }

    #[test]
    fn test_config_file() {
        assert_eq!(
            FileRole::detect("app.config.rs", Language::Rust),
            FileRole::Config
        );
        assert_eq!(
            FileRole::detect("src/config/settings.rs", Language::Rust),
            FileRole::Config
        );
    }

    #[test]
    fn test_parse_roundtrip() {
        for role in [
            FileRole::Implementation,
            FileRole::Test,
            FileRole::Fixture,
            FileRole::Generated,
            FileRole::Config,
        ] {
            assert_eq!(FileRole::parse(role.as_str()), Some(role));
        }
    }

    #[test]
    fn test_display() {
        assert_eq!(FileRole::Implementation.to_string(), "implementation");
        assert_eq!(FileRole::Test.to_string(), "test");
    }
}
