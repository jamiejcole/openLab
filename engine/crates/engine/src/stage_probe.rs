// SPDX-License-Identifier: GPL-3.0-or-later

//! The first look at a video file or still image: a throwaway stage host process
//! (`openlab-stage --probe <file>`) reads it and prints one line (`video_probe::ProbeReport`).
//! The engine never opens such a file itself, and the platform's decoder never runs in the stage
//! host that draws the show: a damaged file that crashes or hangs it takes only that process.
//!
//! Called on a probe worker (never the control thread). The process is killed if it has not
//! answered within the timeout, and whatever it printed is validated before it is believed.

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use openlab_audio::MediaError;
use openlab_core::video_probe::{MAX_PROBE_LINE_BYTES, ProbeReport, decode_probe_report};

/// How long one probe process may take before it is killed and the file reported unreadable:
/// opening a movie and decoding its first frame takes well under a second on the target Mac;
/// this leaves room for a slow disk without letting a hung decoder keep a worker.
pub const PROBE_PROCESS_LIMIT: Duration = Duration::from_secs(20);

/// What a picture probe found: a video or a still image the stage host can play.
pub type PictureProbe = Arc<dyn Fn(&Path) -> Result<ProbeReport, MediaError> + Send + Sync>;

/// Probes with `program` (the stage host), or, with none, reports that this engine has no video.
pub fn picture_probe(program: Option<PathBuf>, limit: Duration) -> PictureProbe {
    Arc::new(move |file: &Path| match &program {
        Some(program) => probe_file(program, file, limit),
        None => Err(MediaError::Unsupported(
            "this computer's openLab has no video output".into(),
        )),
    })
}

/// Runs `program --probe <file>` and reads its answer. A file that is not there is
/// [`MediaError::NotFound`]; one the probe cannot play, a process that fails, prints nothing
/// valid or takes longer than `limit` is [`MediaError::Unsupported`] with the reason. `Ok` is
/// always a video or a still.
pub fn probe_file(program: &Path, file: &Path, limit: Duration) -> Result<ProbeReport, MediaError> {
    // The engine's own check, so a missing file never needs a process.
    match std::fs::metadata(file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(MediaError::NotFound(file.to_path_buf()));
        }
        Err(e) => return Err(MediaError::Io(e.to_string())),
        Ok(m) if !m.is_file() => return Err(MediaError::NotAFile(file.to_path_buf())),
        Ok(_) => {}
    }
    let mut command = Command::new(program);
    command
        .arg("--probe")
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command
        .spawn()
        .map_err(|e| MediaError::Unsupported(format!("the video probe could not start ({e})")))?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(MediaError::Worker);
    };
    let (tx, rx) = crossbeam_channel::bounded(1);
    // Reads at most one line's limit, so a flood costs nothing.
    let reader = std::thread::Builder::new()
        .name("openlab-picture-probe".into())
        .spawn(move || {
            let mut text = Vec::new();
            let limit = u64::try_from(MAX_PROBE_LINE_BYTES + 1).unwrap_or(u64::MAX);
            let read = BufReader::new(stdout).take(limit).read_to_end(&mut text);
            let _ = tx.send(read.map(|_| text));
        });
    if reader.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(MediaError::Worker);
    }
    let answer = rx.recv_timeout(limit);
    // Ended or not, the process goes now: its line is read (or it has had its time).
    let _ = child.kill();
    let _ = child.wait();
    let text = match answer {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => return Err(MediaError::Io(e.to_string())),
        Err(_) => {
            return Err(MediaError::Unsupported(
                "the file could not be read in time".into(),
            ));
        }
    };
    let line = std::str::from_utf8(&text)
        .ok()
        .and_then(|t| t.lines().next())
        .unwrap_or("");
    match decode_probe_report(line) {
        Ok(ProbeReport::Missing) => Err(MediaError::NotFound(file.to_path_buf())),
        Ok(ProbeReport::Unsupported { reason }) => Err(MediaError::Unsupported(reason)),
        Ok(report) => Ok(report),
        Err(_) => Err(MediaError::Unsupported(
            "the video probe gave no answer (the file may be damaged)".into(),
        )),
    }
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A stand-in probe program running `body` as a shell script, and a file to probe.
    fn stand_in(body: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("probe");
        std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = dir.path().join("clip.mov");
        std::fs::write(&file, b"not really a movie").unwrap();
        (dir, program, file)
    }

    fn reason(r: Result<ProbeReport, MediaError>) -> String {
        match r {
            Err(MediaError::Unsupported(why)) => why,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_good_answer_is_believed_and_a_missing_file_needs_no_process() {
        let (_dir, program, file) = stand_in(
            r#"echo '{"type":"video","codec":"h264","width":1920,"height":1080,"fps":25.0,"duration":2.0}'"#,
        );
        let report = probe_file(&program, &file, Duration::from_secs(10)).unwrap();
        assert!(matches!(report, ProbeReport::Video { width: 1920, .. }));
        let gone = file.with_file_name("gone.mov");
        assert!(matches!(
            probe_file(&program, &gone, Duration::from_secs(10)),
            Err(MediaError::NotFound(_))
        ));
        let folder = file.with_file_name("");
        assert!(matches!(
            probe_file(&program, &folder, Duration::from_secs(10)),
            Err(MediaError::NotAFile(_))
        ));
    }

    #[test]
    fn the_probe_saying_unsupported_or_missing_is_passed_on() {
        let (_dir, program, file) =
            stand_in(r#"echo '{"type":"unsupported","reason":"its codec is not supported"}'"#);
        assert_eq!(
            reason(probe_file(&program, &file, Duration::from_secs(10))),
            "its codec is not supported"
        );
        let (_dir, program, file) = stand_in(r#"echo '{"type":"missing"}'"#);
        assert!(matches!(
            probe_file(&program, &file, Duration::from_secs(10)),
            Err(MediaError::NotFound(_))
        ));
    }

    /// A probe process that crashes, prints garbage, floods its output, prints a value out of
    /// range or never answers gives a broken cue with a reason, and never holds the worker
    /// past the limit.
    #[test]
    fn a_crashing_lying_flooding_or_hung_probe_is_a_reason_never_a_hang() {
        let limit = Duration::from_millis(500);
        for body in [
            "kill -SEGV $$",
            "exit 3",
            "echo 'not json'",
            r#"echo '{"type":"video","codec":"h264","width":0,"height":0,"fps":25.0,"duration":2.0}'"#,
            "yes '{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{'",
            "exec sleep 30",
        ] {
            // A hung one returns at the limit (the process killed), or this test would hang.
            let (_dir, program, file) = stand_in(body);
            let why = reason(probe_file(&program, &file, limit));
            assert!(!why.is_empty(), "{body}");
        }
    }

    #[test]
    fn a_probe_that_cannot_start_or_an_engine_without_video_is_a_reason() {
        let (dir, _program, file) = stand_in("true");
        let why = reason(probe_file(
            &dir.path().join("no-such-program"),
            &file,
            Duration::from_secs(1),
        ));
        assert!(why.contains("could not start"), "{why}");
        let none = picture_probe(None, Duration::from_secs(1));
        assert!(reason(none(&file)).contains("no video output"));
    }
}
