use std::fs::File;
use std::io::prelude::*;
use std::io::{self, BufReader, BufWriter, ErrorKind};
use std::path::Path;
use std::process::Command;
use std::string;

use mktemp::Temp;

quick_error! {
    #[derive(Debug)]
    pub enum Error {
        UnableToCreateTempDir(err: io::Error) {
            description("unable to create temporary directory")
            display("Unable to create temporary directory: {}", err)
            cause(err)
        }
        UnableToCreateSourceFile(err: io::Error) {
            description("unable to create source file")
            display("Unable to create source file: {}", err)
            cause(err)
        }
        UnableToExecuteCompiler(err: io::Error) {
            description("unable to execute the compiler")
            display("Unable to execute the compiler: {}", err)
            cause(err)
        }
        UnableToReadOutput(err: io::Error) {
            description("unable to read output file")
            display("Unable to read output file: {}", err)
            cause(err)
        }
        OutputNotUtf8(err: string::FromUtf8Error) {
            description("output was not valid UTF-8")
            display("Output was not valid UTF-8: {}", err)
            cause(err)
        }
        OutputMissing {
            description("output was missing")
            display("Output was missing")
        }
    }
}

pub type Result<T> = ::std::result::Result<T, Error>;

pub struct Sandbox {
    input_file: Temp,
    output_dir: Temp,
}

fn vec_to_str(v: Vec<u8>) -> Result<String> {
    String::from_utf8(v).map_err(Error::OutputNotUtf8)
}

impl Sandbox {
    pub fn new() -> Result<Self> {
        Ok(Sandbox {
            input_file: try!(Temp::new_file().map_err(Error::UnableToCreateTempDir)),
            output_dir: try!(Temp::new_dir().map_err(Error::UnableToCreateTempDir)),
        })
    }

    pub fn compile(&self, req: &CompileRequest) -> Result<CompileResponse> {
        try!(self.write_source_code(&req.code));

        let mut command = self.compile_command(req.target, req.channel, req.mode, req.tests);

        let output = try!(command.output().map_err(Error::UnableToExecuteCompiler));

        let mut result_path = self.output_dir.as_ref().to_path_buf();
        match req.target {
            CompileTarget::Assembly => result_path.push("compilation.s"),
            CompileTarget::LlvmIr   => result_path.push("compilation.ll"),
        }

        Ok(CompileResponse {
            success: output.status.success(),
            code: try!(read(&result_path)).unwrap_or_else(String::new),
            stdout: try!(vec_to_str(output.stdout)),
            stderr: try!(vec_to_str(output.stderr)),
        })
    }

    pub fn execute(&self, req: &ExecuteRequest) -> Result<ExecuteResponse> {
        try!(self.write_source_code(&req.code));
        let mut command = self.execute_command(req.channel, req.mode, req.tests);

        let output = try!(command.output().map_err(Error::UnableToExecuteCompiler));

        Ok(ExecuteResponse {
            success: output.status.success(),
            stdout: try!(vec_to_str(output.stdout)),
            stderr: try!(vec_to_str(output.stderr)),
        })
    }

    pub fn format(&self, req: &FormatRequest) -> Result<FormatResponse> {
        try!(self.write_source_code(&req.code));
        let mut command = self.format_command();

        let output = try!(command.output().map_err(Error::UnableToExecuteCompiler));

        Ok(FormatResponse {
            success: output.status.success(),
            code: try!(try!(read(self.input_file.as_ref())).ok_or(Error::OutputMissing)),
            stdout: try!(vec_to_str(output.stdout)),
            stderr: try!(vec_to_str(output.stderr)),
        })
    }

    pub fn clippy(&self, req: &ClippyRequest) -> Result<ClippyResponse> {
        try!(self.write_source_code(&req.code));
        let mut command = self.clippy_command();

        let output = try!(command.output().map_err(Error::UnableToExecuteCompiler));

        Ok(ClippyResponse {
            success: output.status.success(),
            stdout: try!(vec_to_str(output.stdout)),
            stderr: try!(vec_to_str(output.stderr)),
        })
    }

    fn write_source_code(&self, code: &str) -> Result<()> {
        let data = code.as_bytes();

        let path = self.input_file.as_ref();
        let file = try!(File::create(path).map_err(Error::UnableToCreateSourceFile));
        let mut file = BufWriter::new(file);

        try!(file.write_all(data).map_err(Error::UnableToCreateSourceFile));

        debug!("Wrote {} bytes of source to {}", data.len(), path.display());
        Ok(())
    }

    fn compile_command(&self, target: CompileTarget, channel: Channel, mode: Mode, tests: bool) -> Command {
        let mut cmd = self.docker_command();

        let execution_cmd = build_execution_command(Some(target), mode, tests);

        cmd.arg(&channel.container_name()).args(&execution_cmd);

        debug!("Compilation command is {:?}", cmd);

        cmd
    }

    fn execute_command(&self, channel: Channel, mode: Mode, tests: bool) -> Command {
        let mut cmd = self.docker_command();

        let execution_cmd = build_execution_command(None, mode, tests);

        cmd.arg(&channel.container_name()).args(&execution_cmd);

        debug!("Execution command is {:?}", cmd);

        cmd
    }

    fn format_command(&self) -> Command {
        let mut cmd = self.docker_command();

        cmd.arg("rustfmt").args(&["--write-mode", "overwrite", "src/main.rs"]);

        debug!("Formatting command is {:?}", cmd);

        cmd
    }

    fn clippy_command(&self) -> Command {
        let mut cmd = self.docker_command();

        cmd.arg("clippy").args(&["cargo", "clippy"]);

        debug!("Clippy command is {:?}", cmd);

        cmd
    }

    fn docker_command(&self) -> Command {
        let mut mount_input_file = self.input_file.as_ref().as_os_str().to_os_string();
        mount_input_file.push(":");
        mount_input_file.push("/playground/src/main.rs");

        let mut mount_output_dir = self.output_dir.as_ref().as_os_str().to_os_string();
        mount_output_dir.push(":");
        mount_output_dir.push("/playground-result");

        let mut cmd = Command::new("docker");

        cmd
            .arg("run")
            .arg("--rm")
            .arg("--volume").arg(&mount_input_file)
            .arg("--volume").arg(&mount_output_dir)
            .args(&["--workdir", "/playground"])
            .args(&["--net", "none"])
            .args(&["--memory", "256m"])
            .args(&["--memory-swap", "320m"])
            .args(&["--env", "PLAYGROUND_TIMEOUT=10"])
            .args(&["--env", "RUST_BACKTRACE=1"]);

        if cfg!(feature = "fork-bomb-prevention") {
            cmd.args(&["--pids-limit", "512"]);
        }

        cmd
    }
}

fn build_execution_command(target: Option<CompileTarget>, mode: Mode, tests: bool) -> Vec<&'static str> {
    use self::CompileTarget::*;
    use self::Mode::*;

    let mut cmd = vec!["cargo"];

    match (target, tests) {
        (Some(_), _)  => cmd.push("rustc"),
        (None, true)  => cmd.push("test"),
        (None, false) => cmd.push("run"),
    }

    if mode == Release {
        cmd.push("--release");
    }

    if let Some(target) = target {
        cmd.extend(&["--", "-o", "/playground-result/compilation"]);

        match target {
            Assembly => cmd.push("--emit=asm"),
             LlvmIr  => cmd.push("--emit=llvm-ir"),
         }
    }

    cmd
}

fn read(path: &Path) -> Result<Option<String>> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(ref e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::UnableToReadOutput(e)),
    };
    let mut f = BufReader::new(f);

    let mut s = String::new();
    try!(f.read_to_string(&mut s).map_err(Error::UnableToReadOutput));
    Ok(Some(s))
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CompileTarget {
    Assembly,
    LlvmIr,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
    Nightly,
}

impl Channel {
    fn container_name(&self) -> &'static str {
        use self::Channel::*;

        match *self {
            Stable => "rust-stable",
            Beta => "rust-beta",
            Nightly => "rust-nightly",
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Mode {
    Debug,
    Release,
}

#[derive(Debug, Clone)]
pub struct CompileRequest {
    pub target: CompileTarget,
    pub channel: Channel,
    pub mode: Mode,
    pub tests: bool,
    pub code: String,
}

#[derive(Debug, Clone)]
pub struct CompileResponse {
    pub success: bool,
    pub code: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct ExecuteRequest {
    pub channel: Channel,
    pub mode: Mode,
    pub tests: bool,
    pub code: String,
}

#[derive(Debug, Clone)]
pub struct ExecuteResponse {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct FormatRequest {
    pub code: String,
}

#[derive(Debug, Clone)]
pub struct FormatResponse {
    pub success: bool,
    pub code: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct ClippyRequest {
    pub code: String,
}

#[derive(Debug, Clone)]
pub struct ClippyResponse {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[cfg(test)]
mod test {
    use super::*;

    const HELLO_WORLD_CODE: &'static str = r#"
    fn main() {
        println!("Hello, world!");
    }
    "#;

    #[test]
    fn basic_functionality() {
        let req = ExecuteRequest {
            channel: Channel::Stable,
            mode: Mode::Debug,
            tests: false,
            code: HELLO_WORLD_CODE.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.execute(&req).expect("Unable to execute code");

        assert!(resp.stdout.contains("Hello, world!"));
    }

    const COMPILATION_MODE_CODE: &'static str = r#"
    #[cfg(debug_assertions)]
    fn main() {
        println!("Compiling in debug mode");
    }

    #[cfg(not(debug_assertions))]
    fn main() {
        println!("Compiling in release mode");
    }
    "#;

    #[test]
    fn debug_mode() {
        let req = ExecuteRequest {
            channel: Channel::Stable,
            mode: Mode::Debug,
            tests: false,
            code: COMPILATION_MODE_CODE.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.execute(&req).expect("Unable to execute code");

        assert!(resp.stdout.contains("debug mode"));
    }

    #[test]
    fn release_mode() {
        let req = ExecuteRequest {
            channel: Channel::Stable,
            mode: Mode::Release,
            tests: false,
            code: COMPILATION_MODE_CODE.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.execute(&req).expect("Unable to execute code");

        assert!(resp.stdout.contains("release mode"));
    }

    static VERSION_CODE: &'static str = r#"
    use std::process::Command;

    fn main() {
        let output = Command::new("rustc").arg("--version").output().unwrap();
        let output = String::from_utf8(output.stdout).unwrap();
        println!("{}", output);
    }
    "#;

    #[test]
    fn stable_channel() {
        let req = ExecuteRequest {
            channel: Channel::Stable,
            mode: Mode::Debug,
            tests: false,
            code: VERSION_CODE.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.execute(&req).expect("Unable to execute code");

        assert!(resp.stdout.contains("rustc"));
        assert!(!resp.stdout.contains("beta"));
        assert!(!resp.stdout.contains("nightly"));
    }

    #[test]
    fn beta_channel() {
        let req = ExecuteRequest {
            channel: Channel::Beta,
            mode: Mode::Debug,
            tests: false,
            code: VERSION_CODE.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.execute(&req).expect("Unable to execute code");

        assert!(resp.stdout.contains("rustc"));
        assert!(resp.stdout.contains("beta"));
        assert!(!resp.stdout.contains("nightly"));
    }

    #[test]
    fn nightly_channel() {
        let req = ExecuteRequest {
            channel: Channel::Nightly,
            mode: Mode::Debug,
            tests: false,
            code: VERSION_CODE.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.execute(&req).expect("Unable to execute code");

        assert!(resp.stdout.contains("rustc"));
        assert!(!resp.stdout.contains("beta"));
        assert!(resp.stdout.contains("nightly"));
    }

    #[test]
    fn output_llvm_ir() {
        let req = CompileRequest {
            target: CompileTarget::LlvmIr,
            channel: Channel::Stable,
            mode: Mode::Debug,
            tests: false,
            code: HELLO_WORLD_CODE.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.compile(&req).expect("Unable to compile code");

        assert!(resp.code.contains("ModuleID"));
        assert!(resp.code.contains("target datalayout"));
        assert!(resp.code.contains("target triple"));
    }

    #[test]
    fn output_assembly() {
        let req = CompileRequest {
            target: CompileTarget::Assembly,
            channel: Channel::Stable,
            mode: Mode::Debug,
            tests: false,
            code: HELLO_WORLD_CODE.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.compile(&req).expect("Unable to compile code");

        assert!(resp.code.contains(".text"));
        assert!(resp.code.contains(".file"));
        assert!(resp.code.contains(".section"));
        assert!(resp.code.contains(".align"));
    }

    #[test]
    fn formatting_code() {
        let req = FormatRequest {
            code: "fn foo () { method_call(); }".to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.format(&req).expect("Unable to format code");

        let lines: Vec<_> = resp.code.lines().collect();

        assert_eq!(lines[0], "fn foo() {");
        assert_eq!(lines[1], "    method_call();");
        assert_eq!(lines[2], "}");
    }

    #[test]
    fn linting_code() {
        let code = r#"
        fn main() {
            let a = 0.0 / 0.0;
            println!("NaN is {}", a);
        }
        "#;

        let req = ClippyRequest {
            code: code.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.clippy(&req).expect("Unable to lint code");

        assert!(resp.stderr.contains("warn(eq_op)"));
        assert!(resp.stderr.contains("warn(zero_divided_by_zero)"));
    }

    #[test]
    fn network_connections_are_disabled() {
        let code = r#"
            fn main() {
                match ::std::net::TcpStream::connect("google.com:80") {
                    Ok(_) => println!("Able to connect to the outside world"),
                    Err(e) => println!("Failed to connect {}, {:?}", e, e),
                }
            }
        "#;

        let req = ExecuteRequest {
            channel: Channel::Stable,
            mode: Mode::Debug,
            tests: false,
            code: code.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.execute(&req).expect("Unable to execute code");

        assert!(resp.stdout.contains("Failed to connect"));
    }

    #[test]
    fn memory_usage_is_limited() {
        let code = r#"
            fn main() {
                let megabyte = 1024 * 1024;
                let mut big = vec![0u8; 384 * megabyte];
                *big.last_mut().unwrap() += 1;
            }
        "#;

        let req = ExecuteRequest {
            channel: Channel::Stable,
            mode: Mode::Debug,
            tests: false,
            code: code.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.execute(&req).expect("Unable to execute code");

        assert!(resp.stderr.contains("Killed"));
    }

    #[test]
    fn wallclock_time_is_limited() {
        let code = r#"
            fn main() {
                let a_long_time = std::time::Duration::from_secs(20);
                std::thread::sleep(a_long_time);
            }
        "#;

        let req = ExecuteRequest {
            channel: Channel::Stable,
            mode: Mode::Debug,
            tests: false,
            code: code.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.execute(&req).expect("Unable to execute code");

        assert!(resp.stderr.contains("Killed"));
    }

    #[test]
    fn number_of_pids_is_limited() {
        let forkbomb = r##"
            fn main() {
                ::std::process::Command::new("sh").arg("-c").arg(r#"
                    z() {
                        z&
                        z
                    }
                    z
                "#).status().unwrap();
            }
        "##;

        let req = ExecuteRequest {
            channel: Channel::Stable,
            mode: Mode::Debug,
            tests: false,
            code: forkbomb.to_string(),
        };

        let sb = Sandbox::new().expect("Unable to create sandbox");
        let resp = sb.execute(&req).expect("Unable to execute code");

        println!("{:?}", resp);
        assert!(resp.stderr.contains("Cannot fork"));
    }
}
