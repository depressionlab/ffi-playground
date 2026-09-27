use super::smc::{Smc, SmcError, fourcc_str};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FanMode {
	Auto,
	Manual,
	System,
}

#[derive(Clone)]
pub struct Fan {
	pub index: u32,
	pub name: String,
	pub min: f32,
	pub max: f32,
	pub actual: f32,
	pub target: f32,
	pub mode: FanMode,
	pub md_key: Option<String>,
}

fn key(index: u32, suffix: &str) -> String {
	format!("F{index}{suffix}")
}

fn read_rpm(smc: &Smc, k: &str) -> Option<f32> {
	smc.read(k)
		.ok()
		.and_then(|v| v.as_f32())
		.filter(|r| r.is_finite())
}

fn fan_name(smc: &Smc, index: u32, count: u32) -> String {
	if let Ok(v) = smc.read(&key(index, "ID"))
		&& v.size >= 16
	{
		let name: String = v.bytes[4..16]
			.iter()
			.take_while(|&&b| b != 0)
			.map(|&b| b as char)
			.filter(|c| c.is_ascii_graphic() || *c == ' ')
			.collect();
		let name = name.trim().to_string();
		if !name.is_empty() {
			return name;
		}
	}
	if count == 2 {
		return [String::from("Left Fan"), String::from("Right Fan")][index as usize].clone();
	}
	format!("Fan {}", index + 1)
}

fn probe_md_key(smc: &Smc, index: u32) -> Option<String> {
	[key(index, "md"), key(index, "Md")]
		.into_iter()
		.find(|k| smc.exists(k))
}

pub fn discover(smc: &Smc) -> Result<Vec<Fan>, SmcError> {
	let count = match smc.read("FNum") {
		Ok(v) => v.as_u32().unwrap_or(0),
		Err(SmcError::KeyNotFound) => 0,
		Err(e) => return Err(e),
	};
	let mut fans = Vec::new();
	for index in 0..count {
		let mut fan = Fan {
			index,
			name: fan_name(smc, index, count),
			min: read_rpm(smc, &key(index, "Mn")).unwrap_or(0.0),
			max: read_rpm(smc, &key(index, "Mx")).unwrap_or(6000.0),
			actual: 0.0,
			target: 0.0,
			mode: FanMode::Auto,
			md_key: probe_md_key(smc, index),
		};
		if !(fan.min >= 0.0 && fan.min < fan.max) {
			fan.min = 0.0;
			fan.max = 6000.0;
		}
		refresh(smc, &mut fan);
		fans.push(fan);
	}
	Ok(fans)
}

pub fn refresh(smc: &Smc, fan: &mut Fan) {
	if let Some(rpm) = read_rpm(smc, &key(fan.index, "Ac")) {
		fan.actual = rpm;
	}
	if let Some(rpm) = read_rpm(smc, &key(fan.index, "Tg")) {
		fan.target = rpm;
	}
	fan.mode = read_mode(smc, fan);
}

pub fn read_mode(smc: &Smc, fan: &Fan) -> FanMode {
	fan.md_key.as_ref().map_or_else(
		|| {
			if force_bits(smc).unwrap_or(0) & (1 << fan.index) != 0 {
				FanMode::Manual
			} else {
				FanMode::Auto
			}
		},
		|md| match smc.read(md).ok().and_then(|v| v.as_u32()) {
			Some(1) => FanMode::Manual,
			Some(0) | None => FanMode::Auto,
			Some(_) => FanMode::System,
		},
	)
}

pub fn write_mode(smc: &Smc, fan: &Fan, manual: bool) -> Result<(), SmcError> {
	fan.md_key.as_ref().map_or_else(
		|| set_force_bit(smc, fan.index, manual),
		|md| smc.write(md, &[u8::from(manual)]),
	)
}

fn force_bits(smc: &Smc) -> Result<u16, SmcError> {
	let v = smc.read("FS! ")?;
	if v.size < 2 {
		return Err(SmcError::BadData);
	}
	Ok(u16::from_be_bytes([v.bytes[0], v.bytes[1]]))
}

fn set_force_bit(smc: &Smc, index: u32, on: bool) -> Result<(), SmcError> {
	let bits = force_bits(smc)?;
	let new = if on {
		bits | (1 << index)
	} else {
		bits & !(1 << index)
	};
	smc.write("FS! ", &new.to_be_bytes())
}

pub fn encode_rpm(smc: &Smc, k: &str, rpm: f32) -> Result<Vec<u8>, SmcError> {
	let info = smc.key_info(super::smc::fourcc(k)?)?;
	match fourcc_str(info.data_type).as_str() {
		"flt " => Ok(rpm.to_le_bytes().to_vec()),
		"fpe2" => Ok(((rpm.clamp(0.0, 16383.0) * 4.0) as u16)
			.to_be_bytes()
			.to_vec()),
		_ => Err(SmcError::BadData),
	}
}

pub fn tg_key(fan: &Fan) -> String {
	key(fan.index, "Tg")
}
