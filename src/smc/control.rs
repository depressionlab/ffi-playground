use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::fan::{self, Fan, FanMode};
use super::smc::{Smc, SmcError};

#[derive(Copy, Clone)]
pub enum Cmd {
	SetTarget { index: usize, rpm: f32 },
	SetAuto { index: usize },
	AllAuto,
	QuitRestore,
	QuitKeep,
}

pub enum Note {
	Info(String),
	Error { index: Option<usize>, text: String },
	Applied,
}

#[derive(Clone, Copy, PartialEq)]
enum Desired {
	Untouched,
	Auto,
	Manual(f32),
}

enum WErr {
	Interrupted,
	Msg(String),
}

const SUDO_MSG: &str = "permission denied! restart with `sudo`";
const MODE_RETRIES: u32 = 300;
const MODE_RETRY_DELAY: Duration = Duration::from_millis(100);
const FTST_RETRIES: u32 = 100;
const TG_RETRIES: u32 = 10;
const WRITE_RETRY_DELAY: Duration = Duration::from_millis(50);

pub struct Controller {
	tx: Option<Sender<Cmd>>,
	pub rx: Receiver<Note>,
	quit: Arc<AtomicBool>,
	handle: Option<JoinHandle<()>>,
}

impl Controller {
	pub fn spawn(fans: Vec<Fan>) -> eyre::Result<Self> {
		let smc = Smc::open()?;
		let quit = Arc::new(AtomicBool::new(false));
		let (cmd_tx, cmd_rx) = mpsc::channel();
		let (note_tx, note_rx) = mpsc::channel();
		let worker_quit = quit.clone();
		let handle = thread::spawn(move || {
			Worker {
				smc,
				desired: vec![Desired::Untouched; fans.len()],
				fans,
				notes: note_tx,
				quit: worker_quit,
				ftst_held: false,
				restoring: false,
				keep: false,
			}
			.run(&cmd_rx);
		});
		Ok(Self {
			tx: Some(cmd_tx),
			rx: note_rx,
			quit,
			handle: Some(handle),
		})
	}

	pub fn send(&self, cmd: Cmd) {
		if matches!(cmd, Cmd::QuitRestore | Cmd::QuitKeep) {
			self.quit.store(true, Ordering::Relaxed);
		}
		if let Some(tx) = &self.tx {
			let _ = tx.send(cmd);
		}
	}
}

impl Drop for Controller {
	fn drop(&mut self) {
		self.quit.store(true, Ordering::Relaxed);
		drop(self.tx.take());
		if let Some(handle) = self.handle.take()
			&& let Err(payload) = handle.join()
		{
			let msg = payload
				.downcast_ref::<&str>()
				.map(ToString::to_string)
				.or_else(|| payload.downcast_ref::<String>().cloned())
				.unwrap_or_else(|| "unknown panic".into());
			eprintln!("smc: fan control thread panicked: {msg}");
		}
	}
}

struct Worker {
	smc: Smc,
	fans: Vec<Fan>,
	desired: Vec<Desired>,
	notes: Sender<Note>,
	quit: Arc<AtomicBool>,
	ftst_held: bool,
	restoring: bool,
	keep: bool,
}

impl Drop for Worker {
	fn drop(&mut self) {
		if !self.keep {
			self.restore_all();
		}
	}
}

impl Worker {
	fn run(mut self, cmds: &Receiver<Cmd>) {
		loop {
			match cmds.recv_timeout(Duration::from_secs(2)) {
				Ok(Cmd::QuitKeep) => {
					self.keep = true;
					return;
				}
				Ok(Cmd::QuitRestore) | Err(RecvTimeoutError::Disconnected) => return,
				Ok(cmd) => self.handle(cmd),
				Err(RecvTimeoutError::Timeout) => self.reassert(),
			}
		}
	}

	fn interrupted(&self) -> bool {
		!self.restoring && self.quit.load(Ordering::Relaxed)
	}

	fn wait(&self, d: Duration) -> bool {
		let deadline = Instant::now() + d;
		loop {
			if self.interrupted() {
				return false;
			}
			let left = deadline.saturating_duration_since(Instant::now());
			if left.is_zero() {
				return true;
			}
			thread::sleep(left.min(Duration::from_millis(50)));
		}
	}

	fn info(&self, msg: impl Into<String>) {
		let _ = self.notes.send(Note::Info(msg.into()));
	}

	fn handle(&mut self, cmd: Cmd) {
		let index = match &cmd {
			Cmd::SetTarget { index, .. } | Cmd::SetAuto { index } => Some(*index),
			_ => None,
		};
		let result = match cmd {
			Cmd::SetTarget { index, rpm } => self.set_target(index, rpm),
			Cmd::SetAuto { index } => self.set_auto(index),
			Cmd::AllAuto => self.all_auto(),
			Cmd::QuitRestore | Cmd::QuitKeep => Ok(()),
		};
		match result {
			Ok(()) => {
				let _ = self.notes.send(Note::Applied);
			}
			Err(WErr::Interrupted) => {}
			Err(WErr::Msg(text)) => {
				let _ = self.notes.send(Note::Error { index, text });
			}
		}
	}

	fn set_target(&mut self, index: usize, rpm: f32) -> Result<(), WErr> {
		if self.interrupted() {
			return Err(WErr::Interrupted);
		}
		let fan = &self.fans[index];
		let rpm = rpm.clamp(fan.min.max(0.0), fan.max);
		self.engage_manual(index)?;
		self.desired[index] = Desired::Manual(rpm);
		self.write_tg(index, rpm)
	}

	fn set_auto(&mut self, index: usize) -> Result<(), WErr> {
		if self.interrupted() {
			return Err(WErr::Interrupted);
		}
		self.desired[index] = Desired::Auto;
		fan_to_auto(&self.smc, &self.fans[index]).map_err(|e| werr(&e, "disable manual mode"))?;
		self.maybe_release_ftst();
		Ok(())
	}

	fn all_auto(&mut self) -> Result<(), WErr> {
		let mut problems = Vec::new();
		for index in 0..self.fans.len() {
			match self.set_auto(index) {
				Ok(()) => {}
				Err(WErr::Interrupted) => return Err(WErr::Interrupted),
				Err(WErr::Msg(m)) => problems.push(format!("{}: {m}", self.fans[index].name)),
			}
		}
		if problems.is_empty() {
			Ok(())
		} else {
			Err(WErr::Msg(problems.join("; ")))
		}
	}

	fn engage_manual(&mut self, index: usize) -> Result<(), WErr> {
		if fan::read_mode(&self.smc, &self.fans[index]) == FanMode::Manual {
			return Ok(());
		}
		match fan::write_mode(&self.smc, &self.fans[index], true) {
			Ok(()) => Ok(()),
			Err(SmcError::NotPrivileged) => Err(WErr::Msg(SUDO_MSG.into())),
			Err(first) => {
				if !self.smc.exists("Ftst") {
					return Err(werr(&first, "enable manual mode"));
				}
				self.info("unlocking fan control from thermal manager (takes 3-6s)...");
				self.write_retry("Ftst", &[1], FTST_RETRIES)
					.map_err(|e| werr(&e, "unlock (Ftst)"))?;
				self.ftst_held = true;
				if !self.wait(Duration::from_secs(3)) {
					return Err(WErr::Interrupted);
				}
				for attempt in 0..MODE_RETRIES {
					match fan::write_mode(&self.smc, &self.fans[index], true) {
						Ok(()) => return Ok(()),
						Err(e) if attempt + 1 == MODE_RETRIES => {
							return Err(werr(&e, "enable manual mode after unlock"));
						}
						Err(_) => {
							if !self.wait(MODE_RETRY_DELAY) {
								return Err(WErr::Interrupted);
							}
						}
					}
				}
				unreachable!()
			}
		}
	}

	fn write_tg(&self, index: usize, rpm: f32) -> Result<(), WErr> {
		let key = fan::tg_key(&self.fans[index]);
		let data = fan::encode_rpm(&self.smc, &key, rpm).map_err(|e| werr(&e, "encode target"))?;
		self.write_retry(&key, &data, TG_RETRIES)
			.map_err(|e| werr(&e, "set target speed"))
	}

	fn write_retry(&self, key: &str, data: &[u8], attempts: u32) -> Result<(), SmcError> {
		retry_write(&self.smc, key, data, attempts, || self.interrupted())
	}

	fn maybe_release_ftst(&mut self) {
		if !self.ftst_held {
			return;
		}
		let any_desired_manual = self.desired.iter().any(|d| matches!(d, Desired::Manual(_)));
		let any_hw_manual = self
			.fans
			.iter()
			.any(|f| fan::read_mode(&self.smc, f) == FanMode::Manual);
		if !any_desired_manual && !any_hw_manual {
			let _ = self.smc.write("Ftst", &[0]);
			self.ftst_held = false;
		}
	}

	fn reassert(&mut self) {
		if self.quit.load(Ordering::Relaxed) {
			return;
		}
		for index in 0..self.fans.len() {
			let outcome = match self.desired[index] {
				Desired::Untouched => continue,
				Desired::Manual(rpm) => {
					if fan::read_mode(&self.smc, &self.fans[index]) == FanMode::Manual {
						continue;
					}
					self.info(format!(
						"system reclaimed {}! re-engaging manual control...",
						self.fans[index].name
					));
					self.engage_manual(index)
						.and_then(|()| self.write_tg(index, rpm))
				}
				Desired::Auto => {
					if fan::read_mode(&self.smc, &self.fans[index]) != FanMode::Manual {
						continue;
					}
					let result = fan_to_auto(&self.smc, &self.fans[index])
						.map_err(|e| werr(&e, "disable manual mode"));
					if result.is_ok() {
						self.maybe_release_ftst();
					}
					result
				}
			};
			match outcome {
				Ok(()) => {
					let _ = self.notes.send(Note::Applied);
				}
				Err(WErr::Interrupted) => return,
				Err(WErr::Msg(text)) => {
					let _ = self.notes.send(Note::Error {
						index: Some(index),
						text,
					});
				}
			}
		}
	}

	fn restore_all(&mut self) {
		self.restoring = true;
		for index in 0..self.fans.len() {
			if self.desired[index] != Desired::Untouched {
				let _ = fan_to_auto(&self.smc, &self.fans[index]);
				self.desired[index] = Desired::Untouched;
			}
		}
		if self.ftst_held {
			let _ = self.smc.write("Ftst", &[0]);
			self.ftst_held = false;
		}
		self.restoring = false;
	}
}

fn retry_write(
	smc: &Smc,
	key: &str,
	data: &[u8],
	attempts: u32,
	abort: impl Fn() -> bool,
) -> Result<(), SmcError> {
	let mut last = SmcError::BadData;
	for _ in 0..attempts {
		if abort() {
			return Err(SmcError::Interrupted);
		}
		match smc.write(key, data) {
			Ok(()) => return Ok(()),
			Err(SmcError::NotPrivileged) => return Err(SmcError::NotPrivileged),
			Err(e) => last = e,
		}
		thread::sleep(WRITE_RETRY_DELAY);
	}
	Err(last)
}

fn fan_to_auto(smc: &Smc, fan: &Fan) -> Result<(), SmcError> {
	fan::write_mode(smc, fan, false)?;
	let tg = fan::tg_key(fan);
	if let Ok(data) = fan::encode_rpm(smc, &tg, 0.0) {
		let _ = retry_write(smc, &tg, &data, TG_RETRIES, || false);
	}
	Ok(())
}

fn werr(e: &SmcError, action: &str) -> WErr {
	match e {
		SmcError::Interrupted => WErr::Interrupted,
		SmcError::NotPrivileged => WErr::Msg(SUDO_MSG.into()),
		SmcError::SmcResult(0x82) => WErr::Msg(format!(
			"could not {action}: rejected by thermal manager (SMC 0x82)"
		)),
		SmcError::SmcResult(0x86) => WErr::Msg(format!("could not {action}: key is not writable")),
		e => WErr::Msg(format!("could not {action}: {e}")),
	}
}

pub fn force_auto(smc: &Smc, fans: &[Fan]) -> Vec<String> {
	let mut problems = Vec::new();
	for fan in fans {
		if let Err(e) = fan_to_auto(smc, fan) {
			let text = match werr(&e, "set auto mode") {
				WErr::Msg(m) => m,
				WErr::Interrupted => continue,
			};
			problems.push(format!("{}: {text}", fan.name));
		}
	}
	if problems.is_empty()
		&& smc.exists("Ftst")
		&& smc.read("Ftst").ok().and_then(|v| v.as_u32()) == Some(1)
		&& let Err(e) = smc.write("Ftst", &[0])
	{
		problems.push(format!(
			"could not release thermal-manager unlock (Ftst): {e}"
		));
	}
	problems
}
