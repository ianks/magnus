use std::{collections::HashMap, env, error::Error, ffi::OsStr, fmt, process::Command};

const RUBY_VERSIONS: [(u8, u8); 3] = [(2, 7), (3, 0), (3, 1)];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=RUBY");

    let rbconfig = RbConfig::new()?;

    let version_parts = rbconfig
        .get("RUBY_API_VERSION")?
        .split('.')
        .map(|s| s.parse::<u8>())
        .collect::<Result<Vec<u8>, _>>()?;
    let version = (version_parts[0], version_parts[1]);
    for &v in &RUBY_VERSIONS {
        if version < v {
            println!(r#"cargo:rustc-cfg=ruby_lt_{}_{}"#, v.0, v.1);
        }
        if version <= v {
            println!(r#"cargo:rustc-cfg=ruby_lte_{}_{}"#, v.0, v.1);
        }
        if version == v {
            println!(r#"cargo:rustc-cfg=ruby_{}_{}"#, v.0, v.1);
        }
        if version >= v {
            println!(r#"cargo:rustc-cfg=ruby_gte_{}_{}"#, v.0, v.1);
        }
        if version > v {
            println!(r#"cargo:rustc-cfg=ruby_gt_{}_{}"#, v.0, v.1);
        }
    }

    if cfg!(windows) {
        println!("cargo:rustc-link-search={}", rbconfig.get("libdir")?);
    }

    Ok(())
}

fn use_static(rbconfig: &RbConfig) -> Result<(), RbConfigMissing> {
    let libs = rbconfig.get("LIBS")?;
    println!("cargo:rustc-link-lib=static=ruby-static");
    println!("cargo:rustc-flags={}", libs.replace("-l", "-l "));
    Ok(())
}

struct RbConfig(HashMap<String, String>);

impl RbConfig {
    fn new() -> Result<Self, RbConfigError> {
        let ruby = match env::var_os("RUBY") {
            Some(val) => val,
            None => OsStr::new("ruby").to_os_string(),
        };
        let output = Command::new(ruby)
            .arg("-e")
            .arg("print RbConfig::CONFIG.map {|kv| kv.join(\"\x1F\")}.join(\"\x1E\")")
            .output()?;
        let config = String::from_utf8(output.stdout)?;

        let mut res = HashMap::new();
        for line in config.split('\x1E') {
            let mut parts = line.splitn(2, '\x1F');
            if let (Some(key), Some(val)) = (parts.next(), parts.next()) {
                if !val.is_empty() {
                    res.insert(key.to_owned(), val.to_owned());
                }
            }
        }
        Ok(RbConfig(res))
    }

    fn get(&self, key: &str) -> Result<&str, RbConfigMissing> {
        self.0
            .get(key)
            .map(|v| v.as_str())
            .ok_or_else(|| RbConfigMissing(key.to_owned()))
    }
}

#[derive(Debug)]
enum RbConfigError {
    Io(std::io::Error),
    Utf8(std::string::FromUtf8Error),
}

impl fmt::Display for RbConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Utf8(e) => e.fmt(f),
        }
    }
}

impl Error for RbConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Utf8(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for RbConfigError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<std::string::FromUtf8Error> for RbConfigError {
    fn from(e: std::string::FromUtf8Error) -> Self {
        Self::Utf8(e)
    }
}

#[derive(Debug)]
struct RbConfigMissing(String);

impl fmt::Display for RbConfigMissing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Couldn't find {:?} in RbConfig", self.0)
    }
}

impl Error for RbConfigMissing {}

#[derive(Debug)]
struct BindingError();

impl fmt::Display for BindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to build bindings")
    }
}

impl Error for BindingError {}
