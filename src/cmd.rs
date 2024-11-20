use async_process::{Command, Stdio};

pub struct CmdBuilder {
    binary_path: String,
    cmd: Vec<String>,
}

impl CmdBuilder {
    pub fn new(binary_path: impl Into<String>) -> Self {
        Self {
            binary_path: binary_path.into(),
            cmd: Vec::new(),
        }
    }
    pub fn arg(&mut self, arg: impl Into<String>) -> &mut Self {
        self.cmd.push(arg.into());
        self
    }

    pub fn args<I: IntoIterator<Item = S>, S: Into<String>>(&mut self, args: I) -> &mut Self {
        self.cmd.extend(args.into_iter().map(|s| s.into()));
        self
    }

    pub fn get_args(&self) -> Vec<String> {
        self.cmd.clone()
    }

    pub fn command(&self) -> Command {
        let mut cmd = Command::new(self.binary_path.clone());

        {
            cmd.stdout(Stdio::piped())
                .stdin(Stdio::piped())
                .stderr(Stdio::piped())
                .args(self.get_args());
        }

        cmd
    }
}
