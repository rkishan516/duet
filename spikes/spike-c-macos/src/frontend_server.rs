//! Drives a persistent `frontend_server` (the Dart CFE compiler daemon)
//! process over its stdin/stdout line protocol, in `--incremental` mode.
//! Protocol reverse-engineered from the tool's own `--help` output plus
//! manual testing (see FINDINGS.md for the exact exchange and a couple of
//! surprises versus the documented format).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

pub struct FrontendServer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    output_dill: String,
    boundary_counter: u64,
}

#[derive(Debug)]
pub struct CompileResult {
    pub ok: bool,
    pub diagnostic_lines: Vec<String>,
    pub elapsed: Duration,
}

pub struct FrontendServerConfig<'a> {
    pub dartaotruntime: &'a str,
    pub snapshot: &'a str,
    pub sdk_root: &'a str,
    pub packages: &'a str,
    pub output_dill: &'a str,
}

impl FrontendServer {
    pub fn spawn(cfg: &FrontendServerConfig) -> Self {
        let mut child = Command::new(cfg.dartaotruntime)
            .arg(cfg.snapshot)
            .arg("--sdk-root")
            .arg(cfg.sdk_root)
            .arg("--incremental")
            .arg("--target=flutter")
            // Matches flutter_tools' default debug-mode resident compiler flags
            // (buildInfo.trackWidgetCreation defaults true for debug builds).
            .arg("--track-widget-creation")
            .arg("--experimental-emit-debug-metadata")
            // The full transitive-dependency dump (hundreds of "+file://..." lines
            // per compile) is on by default and swamps the protocol parser for no
            // benefit here - see FINDINGS.md.
            .arg("--no-print-incremental-dependencies")
            .arg("--packages")
            .arg(cfg.packages)
            .arg("--output-dill")
            .arg(cfg.output_dill)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr goes straight to fd 2 (our real terminal), independent of the
            // stdout_capture trick applied to THIS process's own fd 1.
            .stderr(Stdio::inherit())
            .spawn()
            .expect("failed to spawn frontend_server");
        let stdin = child.stdin.take().expect("frontend_server stdin missing");
        let stdout = BufReader::new(child.stdout.take().expect("frontend_server stdout missing"));
        Self {
            child,
            stdin,
            stdout,
            output_dill: cfg.output_dill.to_string(),
            boundary_counter: 0,
        }
    }

    fn next_boundary(&mut self) -> String {
        self.boundary_counter += 1;
        format!("spike-c-boundary-{}", self.boundary_counter)
    }

    /// Reads lines until one starting with `"result "` appears (extracting the
    /// boundary key frontend_server generated for this response), then keeps
    /// reading - collecting everything as "diagnostics" - until a line
    /// starting with that same boundary key appears again (the terminator).
    ///
    /// Any lines seen BEFORE the "result " line are discarded as stray/delayed
    /// output from a previous `accept` (see FINDINGS.md: `accept` appears to
    /// echo back a confirmation line asynchronously, which can arrive
    /// interleaved with the next command's response rather than immediately).
    fn read_result_block(&mut self) -> CompileResult {
        let start = Instant::now();
        let mut boundary: Option<String> = None;
        let mut diagnostics = Vec::new();
        loop {
            let mut line = String::new();
            let n = self
                .stdout
                .read_line(&mut line)
                .expect("frontend_server stdout read failed");
            if n == 0 {
                panic!(
                    "frontend_server stdout hit EOF unexpectedly (process likely died) - \
                     diagnostics so far: {diagnostics:?}"
                );
            }
            let line = line.trim_end_matches(['\n', '\r']).to_string();
            match &boundary {
                None => {
                    if let Some(rest) = line.strip_prefix("result ") {
                        boundary = Some(rest.trim().to_string());
                    }
                    // else: stray line, discard.
                }
                Some(b) => {
                    if line.starts_with(b.as_str()) {
                        let ok = !diagnostics.iter().any(|l: &String| {
                            l.contains("Error:") || l.to_lowercase().contains("exception")
                        });
                        return CompileResult {
                            ok,
                            diagnostic_lines: diagnostics,
                            elapsed: start.elapsed(),
                        };
                    }
                    diagnostics.push(line);
                }
            }
        }
    }

    /// Initial full compile, establishing frontend_server's baseline
    /// understanding of the entrypoint's source. Writes the full kernel to
    /// `--output-dill` (not used directly by the running engine - that
    /// already loaded its own `kernel_blob.bin` from the app bundle at boot -
    /// this call exists purely so future `recompile` calls have a correct
    /// starting point to diff against).
    pub fn compile(&mut self, entrypoint_file_uri: &str) -> CompileResult {
        writeln!(self.stdin, "compile {entrypoint_file_uri}").expect("write compile failed");
        self.stdin.flush().ok();
        self.read_result_block()
    }

    /// Commits the most recent compile/recompile as the new baseline for the
    /// next incremental diff. Protocol docs list this as a bare `accept`
    /// command with no arguments and no documented response.
    pub fn accept(&mut self) {
        writeln!(self.stdin, "accept").expect("write accept failed");
        self.stdin.flush().ok();
    }

    /// Sends `recompile <boundary>\n<invalidated-uri>\n<boundary>\n` for a
    /// single invalidated file, and returns the result plus the path to the
    /// incremental kernel diff frontend_server wrote. Empirically (see
    /// FINDINGS.md) that path is `<output-dill>.incremental.dill`, regardless
    /// of not being passed `--output-incremental-dill` explicitly.
    pub fn recompile(&mut self, invalidated_file_uri: &str) -> (CompileResult, String) {
        let boundary = self.next_boundary();
        writeln!(self.stdin, "recompile {boundary}").expect("write recompile failed");
        writeln!(self.stdin, "{invalidated_file_uri}").expect("write invalidated uri failed");
        writeln!(self.stdin, "{boundary}").expect("write recompile terminator failed");
        self.stdin.flush().ok();
        let result = self.read_result_block();
        let incremental_dill = format!("{}.incremental.dill", self.output_dill);
        (result, incremental_dill)
    }

    pub fn quit(mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
