use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::mem::size_of;

type KernReturn = i32;
type MachPort = u32;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
	unsafe fn IOServiceMatching(name: *const u8) -> *mut c_void;
	unsafe fn IOServiceGetMatchingService(master_port: MachPort, matching: *mut c_void)
	-> MachPort;
	unsafe fn IOServiceOpen(
		service: MachPort,
		owning_task: MachPort,
		conn_type: u32,
		connect: *mut MachPort,
	) -> KernReturn;
	unsafe fn IOServiceClose(connect: MachPort) -> KernReturn;
	unsafe fn IOObjectRelease(object: MachPort) -> KernReturn;
	unsafe fn IOConnectCallStructMethod(
		connection: MachPort,
		selector: u32,
		input: *const c_void,
		input_cnt: usize,
		output: *mut c_void,
		output_cnt: *mut usize,
	) -> KernReturn;
}

unsafe extern "C" {
	unsafe static mach_task_self_: MachPort;
	unsafe fn geteuid() -> u32;
}

pub(super) fn is_root() -> bool {
	unsafe { geteuid() == 0 }
}

const KERNEL_INDEX_SMC: u32 = 2;
const CMD_READ_BYTES: u8 = 5;
const CMD_WRITE_BYTES: u8 = 6;
const CMD_READ_INDEX: u8 = 8;
const CMD_READ_KEYINFO: u8 = 9;
const RESULT_KEY_NOT_FOUND: u8 = 0x84;
const KIO_NOT_PRIVILEGED: KernReturn = 0xE00002C1u32.cast_signed();

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SmcVersion {
	major: u8,
	minor: u8,
	build: u8,
	reserved: u8,
	release: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SmcPLimitData {
	version: u16,
	length: u16,
	cpu_plimit: u32,
	gpu_plimit: u32,
	mem_plimit: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SmcKeyInfoData {
	data_size: u32,
	data_type: u32,
	data_attributes: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SmcKeyData {
	key: u32,
	vers: SmcVersion,
	p_limit: SmcPLimitData,
	key_info: SmcKeyInfoData,
	result: u8,
	status: u8,
	data8: u8,
	data32: u32,
	bytes: [u8; 32],
}

const _: () = assert!(size_of::<SmcKeyData>() == 80);

#[derive(Debug)]
pub enum SmcError {
	ServiceNotFound,
	OpenFailed(i32),
	Call(i32),
	KeyNotFound,
	NotPrivileged,
	SmcResult(u8),
	BadData,
	Interrupted,
}

impl fmt::Display for SmcError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::ServiceNotFound => write!(f, "AppleSMC service not found"),
			Self::OpenFailed(kr) => write!(f, "failed to open AppleSMC (kern {kr:#x})"),
			Self::Call(kr) => write!(f, "SMC call failed (kern {kr:#x})"),
			Self::KeyNotFound => write!(f, "SMC key not found"),
			Self::NotPrivileged => write!(f, "permission denied (run with sudo)"),
			Self::SmcResult(r) => write!(f, "SMC error result {r:#x}"),
			Self::BadData => write!(f, "unexpected SMC data"),
			Self::Interrupted => write!(f, "interrupted"),
		}
	}
}

impl std::error::Error for SmcError {}

pub const fn fourcc(s: &str) -> Result<u32, SmcError> {
	let b = s.as_bytes();
	if b.len() != 4 {
		return Err(SmcError::BadData);
	}
	Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn fourcc_str(v: u32) -> String {
	String::from_utf8_lossy(&v.to_be_bytes()).into_owned()
}

#[derive(Clone, Copy)]
pub struct KeyInfo {
	pub size: u32,
	pub data_type: u32,
}

pub struct Value {
	pub data_type: u32,
	pub size: u32,
	pub bytes: [u8; 32],
}

impl Value {
	pub fn type_str(&self) -> String {
		fourcc_str(self.data_type)
	}

	pub fn as_f32(&self) -> Option<f32> {
		match self.type_str().as_str() {
			"flt " if self.size >= 4 => Some(f32::from_le_bytes([
				self.bytes[0],
				self.bytes[1],
				self.bytes[2],
				self.bytes[3],
			])),
			"fpe2" if self.size >= 2 => {
				Some(f32::from(u16::from_be_bytes([self.bytes[0], self.bytes[1]])) / 4.0)
			}
			"sp78" if self.size >= 2 => {
				Some(f32::from(i16::from_be_bytes([self.bytes[0], self.bytes[1]])) / 256.0)
			}
			"ui8 " => Some(f32::from(self.bytes[0])),
			"ui16" if self.size >= 2 => Some(f32::from(u16::from_be_bytes([
				self.bytes[0],
				self.bytes[1],
			]))),
			_ => self.as_u32().map(|v| v as f32),
		}
	}

	pub fn as_u32(&self) -> Option<u32> {
		match self.type_str().as_str() {
			"ui8 " => Some(u32::from(self.bytes[0])),
			"ui16" if self.size >= 2 => Some(u32::from(u16::from_be_bytes([
				self.bytes[0],
				self.bytes[1],
			]))),
			"ui32" if self.size >= 4 => Some(u32::from_be_bytes([
				self.bytes[0],
				self.bytes[1],
				self.bytes[2],
				self.bytes[3],
			])),
			_ => None,
		}
	}
}

pub struct Smc {
	conn: MachPort,
	info_cache: RefCell<HashMap<u32, KeyInfo>>,
}

impl Smc {
	pub fn open() -> Result<Self, SmcError> {
		let matching = unsafe { IOServiceMatching(c"AppleSMC".as_ptr().cast::<u8>()) };
		if matching.is_null() {
			return Err(SmcError::ServiceNotFound);
		}
		let service = unsafe { IOServiceGetMatchingService(0, matching) };
		if service == 0 {
			return Err(SmcError::ServiceNotFound);
		}
		let mut conn: MachPort = 0;
		let kr = unsafe { IOServiceOpen(service, mach_task_self_, 0, &raw mut conn) };
		unsafe { IOObjectRelease(service) };
		if kr != 0 {
			return Err(SmcError::OpenFailed(kr));
		}
		Ok(Self {
			conn,
			info_cache: RefCell::new(HashMap::new()),
		})
	}

	fn call(&self, input: &SmcKeyData) -> Result<SmcKeyData, SmcError> {
		let mut output = SmcKeyData::default();
		let mut out_len = size_of::<SmcKeyData>();
		let kr = unsafe {
			IOConnectCallStructMethod(
				self.conn,
				KERNEL_INDEX_SMC,
				std::ptr::from_ref::<SmcKeyData>(input).cast::<c_void>(),
				size_of::<SmcKeyData>(),
				(&raw mut output).cast::<c_void>(),
				&raw mut out_len,
			)
		};
		if kr == KIO_NOT_PRIVILEGED {
			return Err(SmcError::NotPrivileged);
		}
		if kr != 0 {
			return Err(SmcError::Call(kr));
		}
		match output.result {
			0 => Ok(output),
			RESULT_KEY_NOT_FOUND => Err(SmcError::KeyNotFound),
			r => Err(SmcError::SmcResult(r)),
		}
	}

	pub fn key_info(&self, key: u32) -> Result<KeyInfo, SmcError> {
		if let Some(info) = self.info_cache.borrow().get(&key) {
			return Ok(*info);
		}
		let input = SmcKeyData {
			key,
			data8: CMD_READ_KEYINFO,
			..Default::default()
		};
		let out = self.call(&input)?;
		let info = KeyInfo {
			size: out.key_info.data_size,
			data_type: out.key_info.data_type,
		};
		self.info_cache.borrow_mut().insert(key, info);
		Ok(info)
	}

	pub fn exists(&self, key: &str) -> bool {
		fourcc(key).is_ok_and(|k| self.key_info(k).is_ok())
	}

	pub fn read(&self, key: &str) -> Result<Value, SmcError> {
		let k = fourcc(key)?;
		let info = self.key_info(k)?;
		if info.size as usize > 32 {
			return Err(SmcError::BadData);
		}
		let input = SmcKeyData {
			key: k,
			key_info: SmcKeyInfoData {
				data_size: info.size,
				..Default::default()
			},
			data8: CMD_READ_BYTES,
			..Default::default()
		};
		let out = self.call(&input)?;
		Ok(Value {
			data_type: info.data_type,
			size: info.size,
			bytes: out.bytes,
		})
	}

	pub fn write(&self, key: &str, data: &[u8]) -> Result<(), SmcError> {
		let k = fourcc(key)?;
		let info = self.key_info(k)?;
		if info.size as usize != data.len() || data.len() > 32 {
			return Err(SmcError::BadData);
		}
		let mut input = SmcKeyData {
			key: k,
			key_info: SmcKeyInfoData {
				data_size: info.size,
				..Default::default()
			},
			data8: CMD_WRITE_BYTES,
			..Default::default()
		};
		input.bytes[..data.len()].copy_from_slice(data);
		self.call(&input)?;
		Ok(())
	}

	pub fn key_count(&self) -> Result<u32, SmcError> {
		let v = self.read("#KEY")?;
		let be = v.as_u32().ok_or(SmcError::BadData)?;
		if be > 0 && be < 100_000 {
			return Ok(be);
		}
		let le = u32::from_le_bytes([v.bytes[0], v.bytes[1], v.bytes[2], v.bytes[3]]);
		if le > 0 && le < 100_000 {
			return Ok(le);
		}
		Err(SmcError::BadData)
	}

	pub fn key_at(&self, index: u32) -> Result<String, SmcError> {
		let input = SmcKeyData {
			data8: CMD_READ_INDEX,
			data32: index,
			..Default::default()
		};
		let out = self.call(&input)?;
		Ok(fourcc_str(out.key))
	}
}

impl Drop for Smc {
	fn drop(&mut self) {
		unsafe { IOServiceClose(self.conn) };
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fourcc_roundtrip() {
		let k = fourcc("F0Ac").unwrap();
		assert_eq!(fourcc_str(k), "F0Ac");
		assert_eq!(fourcc("#KEY").unwrap(), 0x234B4559);
	}

	#[test]
	fn fpe2_decode() {
		let mut bytes = [0u8; 32];
		bytes[0] = 0x13;
		bytes[1] = 0x88;
		let v = Value {
			data_type: fourcc("fpe2").unwrap(),
			size: 2,
			bytes,
		};
		assert_eq!(v.as_f32(), Some(1250.0));
	}

	#[test]
	fn flt_decode() {
		let mut bytes = [0u8; 32];
		bytes[..4].copy_from_slice(&1296.5f32.to_le_bytes());
		let v = Value {
			data_type: fourcc("flt ").unwrap(),
			size: 4,
			bytes,
		};
		assert_eq!(v.as_f32(), Some(1296.5));
	}
}
