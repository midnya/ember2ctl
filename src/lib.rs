use std::{collections::BTreeSet, fmt, str::FromStr, sync::LazyLock};

use btleplug::api::{CharPropFlags, Characteristic, Peripheral, WriteType};
use log::debug;
use uuid::Uuid;

pub static EMBER_SERVICE_UUID: LazyLock<Uuid> =
    LazyLock::new(|| Uuid::from_str("fc543622-236c-4c94-8fa9-944a3e5353fa").unwrap());

static MUG_NAME_UUID: LazyLock<Uuid> =
    LazyLock::new(|| Uuid::from_str("fc540001-236c-4c94-8fa9-944a3e5353fa").unwrap());
static CURRENT_TEMP_UUID: LazyLock<Uuid> =
    LazyLock::new(|| Uuid::from_str("fc540002-236c-4c94-8fa9-944a3e5353fa").unwrap());
static TARGET_TEMP_UUID: LazyLock<Uuid> =
    LazyLock::new(|| Uuid::from_str("fc540003-236c-4c94-8fa9-944a3e5353fa").unwrap());
static TEMP_UNIT_UUID: LazyLock<Uuid> =
    LazyLock::new(|| Uuid::from_str("fc540004-236c-4c94-8fa9-944a3e5353fa").unwrap());
static LIQUID_LEVEL_UUID: LazyLock<Uuid> =
    LazyLock::new(|| Uuid::from_str("fc540005-236c-4c94-8fa9-944a3e5353fa").unwrap());
static BATTERY_UUID: LazyLock<Uuid> =
    LazyLock::new(|| Uuid::from_str("fc540007-236c-4c94-8fa9-944a3e5353fa").unwrap());
static LIQUID_STATE_UUID: LazyLock<Uuid> =
    LazyLock::new(|| Uuid::from_str("fc540008-236c-4c94-8fa9-944a3e5353fa").unwrap());
static MUG_COLOR_UUID: LazyLock<Uuid> =
    LazyLock::new(|| Uuid::from_str("fc540014-236c-4c94-8fa9-944a3e5353fa").unwrap());

static MUG_NAME_CHAR: LazyLock<Characteristic> =
    LazyLock::new(|| make_char(*MUG_NAME_UUID, CharPropFlags::READ | CharPropFlags::WRITE));
static CURRENT_TEMP_CHAR: LazyLock<Characteristic> =
    LazyLock::new(|| make_char(*CURRENT_TEMP_UUID, CharPropFlags::READ));
static TARGET_TEMP_CHAR: LazyLock<Characteristic> =
    LazyLock::new(|| make_char(*TARGET_TEMP_UUID, CharPropFlags::READ | CharPropFlags::WRITE));
static TEMP_UNIT_CHAR: LazyLock<Characteristic> =
    LazyLock::new(|| make_char(*TEMP_UNIT_UUID, CharPropFlags::READ | CharPropFlags::WRITE));
static LIQUID_LEVEL_CHAR: LazyLock<Characteristic> =
    LazyLock::new(|| make_char(*LIQUID_LEVEL_UUID, CharPropFlags::READ));
static BATTERY_CHAR: LazyLock<Characteristic> =
    LazyLock::new(|| make_char(*BATTERY_UUID, CharPropFlags::READ));
static LIQUID_STATE_CHAR: LazyLock<Characteristic> =
    LazyLock::new(|| make_char(*LIQUID_STATE_UUID, CharPropFlags::READ));
static MUG_COLOR_CHAR: LazyLock<Characteristic> =
    LazyLock::new(|| make_char(*MUG_COLOR_UUID, CharPropFlags::READ | CharPropFlags::WRITE));

fn make_char(uuid: Uuid, properties: CharPropFlags) -> Characteristic {
    Characteristic {
        uuid,
        service_uuid: *EMBER_SERVICE_UUID,
        properties,
        descriptors: BTreeSet::default(),
    }
}

#[derive(Copy, Clone, Debug)]
pub enum CommandKind {
    Request,
    Command,
}

impl From<CommandKind> for WriteType {
    fn from(value: CommandKind) -> Self {
        match value {
            CommandKind::Request => WriteType::WithResponse,
            CommandKind::Command => WriteType::WithoutResponse,
        }
    }
}

pub struct Command {
    target: &'static Characteristic,
    kind: CommandKind,
    payload: Vec<u8>,
}

pub async fn send_command<P: Peripheral>(
    peripheral: &P,
    command: &Command,
) -> Result<(), btleplug::Error> {
    let uuid = command.target.uuid;
    let kind = command.kind;
    let payload = &command.payload;
    debug!("write {uuid} ({kind:?}) payload={payload:02X?}");
    peripheral
        .write(command.target, &command.payload, command.kind.into())
        .await
}

#[derive(Debug)]
pub enum CommandBuilderError {
    /// Target temperature outside the accepted 0 or 50.0–100.0 °C range.
    InvalidTargetTemperature,
    /// Mug name longer than 14 bytes, non-ASCII, or contains a space.
    InvalidMugName,
    /// Decoded characteristic payload did not match the expected length.
    InvalidPayloadLength,
    /// Decoded characteristic payload contained an unrecognised discriminant.
    UnknownDiscriminant(u8),
}

impl fmt::Display for CommandBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandBuilderError::InvalidTargetTemperature => {
                f.write_str("target temperature must be 0 or between 50 and 100 Celsius")
            }
            CommandBuilderError::InvalidMugName => {
                f.write_str("mug name must be 1–14 ASCII characters with no spaces")
            }
            CommandBuilderError::InvalidPayloadLength => {
                f.write_str("characteristic payload too short")
            }
            CommandBuilderError::UnknownDiscriminant(b) => {
                write!(f, "unrecognised payload byte: 0x{b:02X}")
            }
        }
    }
}

impl std::error::Error for CommandBuilderError {}

type CommandResult = Result<Command, CommandBuilderError>;

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TemperatureUnit {
    Celsius = 0,
    Fahrenheit = 1,
}

impl TemperatureUnit {
    fn from_byte(b: u8) -> Result<Self, CommandBuilderError> {
        match b {
            0 => Ok(TemperatureUnit::Celsius),
            1 => Ok(TemperatureUnit::Fahrenheit),
            other => Err(CommandBuilderError::UnknownDiscriminant(other)),
        }
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LiquidState {
    Empty = 1,
    Filling = 2,
    /// Reported by the device but ignored by the official app.
    Unknown = 3,
    Cooling = 4,
    Heating = 5,
    Stable = 6,
}

impl LiquidState {
    fn from_byte(b: u8) -> Result<Self, CommandBuilderError> {
        match b {
            1 => Ok(LiquidState::Empty),
            2 => Ok(LiquidState::Filling),
            3 => Ok(LiquidState::Unknown),
            4 => Ok(LiquidState::Cooling),
            5 => Ok(LiquidState::Heating),
            6 => Ok(LiquidState::Stable),
            other => Err(CommandBuilderError::UnknownDiscriminant(other)),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LiquidLevel {
    Empty,
    NotEmpty,
    /// Raw byte present but outside the documented {0, 30} set.
    Intermediate(u8),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
    pub temperature_c: f32,
}

pub fn set_target_temperature_celsius(celsius: f32) -> CommandResult {
    let raw = encode_target_temperature(celsius)?;
    Ok(Command {
        target: &TARGET_TEMP_CHAR,
        kind: CommandKind::Request,
        payload: raw.to_le_bytes().to_vec(),
    })
}

pub fn disable_heating() -> CommandResult {
    Ok(Command {
        target: &TARGET_TEMP_CHAR,
        kind: CommandKind::Request,
        payload: 0u16.to_le_bytes().to_vec(),
    })
}

pub fn set_temperature_unit(unit: TemperatureUnit) -> CommandResult {
    Ok(Command {
        target: &TEMP_UNIT_CHAR,
        kind: CommandKind::Request,
        payload: vec![unit as u8],
    })
}

pub fn set_mug_color(color: Color) -> CommandResult {
    // The orlopau/ember-mug docs list bytes as R, B, G, A, but the worked
    // example BF FF 00 FF described as "lime-green" only maps to RGB(191,255,0)
    // when interpreted as R, G, B, A. Trust the example over the table.
    Ok(Command {
        target: &MUG_COLOR_CHAR,
        kind: CommandKind::Request,
        payload: vec![color.red, color.green, color.blue, color.alpha],
    })
}

pub fn set_mug_name(name: &str) -> CommandResult {
    if name.is_empty()
        || name.len() > 14
        || !name.is_ascii()
        || name.chars().any(|c| c == ' ')
    {
        return Err(CommandBuilderError::InvalidMugName);
    }
    Ok(Command {
        target: &MUG_NAME_CHAR,
        kind: CommandKind::Request,
        payload: name.as_bytes().to_vec(),
    })
}

fn encode_target_temperature(celsius: f32) -> Result<u16, CommandBuilderError> {
    if celsius == 0.0 {
        return Ok(0);
    }
    if !(celsius.is_finite() && (50.0..=100.0).contains(&celsius)) {
        return Err(CommandBuilderError::InvalidTargetTemperature);
    }
    Ok((celsius * 100.0).round() as u16)
}

async fn read_logged<P: Peripheral>(
    peripheral: &P,
    char: &Characteristic,
) -> Result<Vec<u8>, btleplug::Error> {
    let bytes = peripheral.read(char).await?;
    let uuid = char.uuid;
    debug!("read  {uuid} -> {bytes:02X?}");
    Ok(bytes)
}

pub async fn read_current_temperature_celsius<P: Peripheral>(
    peripheral: &P,
) -> Result<Result<f32, CommandBuilderError>, btleplug::Error> {
    let bytes = read_logged(peripheral, &CURRENT_TEMP_CHAR).await?;
    Ok(decode_temperature(&bytes))
}

pub async fn read_target_temperature_celsius<P: Peripheral>(
    peripheral: &P,
) -> Result<Result<f32, CommandBuilderError>, btleplug::Error> {
    let bytes = read_logged(peripheral, &TARGET_TEMP_CHAR).await?;
    Ok(decode_temperature(&bytes))
}

pub async fn read_temperature_unit<P: Peripheral>(
    peripheral: &P,
) -> Result<Result<TemperatureUnit, CommandBuilderError>, btleplug::Error> {
    let bytes = read_logged(peripheral, &TEMP_UNIT_CHAR).await?;
    Ok(bytes
        .first()
        .copied()
        .ok_or(CommandBuilderError::InvalidPayloadLength)
        .and_then(TemperatureUnit::from_byte))
}

pub async fn read_liquid_level<P: Peripheral>(
    peripheral: &P,
) -> Result<Result<LiquidLevel, CommandBuilderError>, btleplug::Error> {
    let bytes = read_logged(peripheral, &LIQUID_LEVEL_CHAR).await?;
    Ok(match bytes.first().copied() {
        None => Err(CommandBuilderError::InvalidPayloadLength),
        Some(0) => Ok(LiquidLevel::Empty),
        Some(30) => Ok(LiquidLevel::NotEmpty),
        Some(other) => Ok(LiquidLevel::Intermediate(other)),
    })
}

pub async fn read_liquid_state<P: Peripheral>(
    peripheral: &P,
) -> Result<Result<LiquidState, CommandBuilderError>, btleplug::Error> {
    let bytes = read_logged(peripheral, &LIQUID_STATE_CHAR).await?;
    Ok(bytes
        .first()
        .copied()
        .ok_or(CommandBuilderError::InvalidPayloadLength)
        .and_then(LiquidState::from_byte))
}

pub async fn read_battery<P: Peripheral>(
    peripheral: &P,
) -> Result<Result<Battery, CommandBuilderError>, btleplug::Error> {
    let bytes = read_logged(peripheral, &BATTERY_CHAR).await?;
    if bytes.len() < 4 {
        return Ok(Err(CommandBuilderError::InvalidPayloadLength));
    }
    Ok(Ok(Battery {
        percent: bytes[0],
        charging: bytes[1] == 1,
        temperature_c: u16::from_le_bytes([bytes[2], bytes[3]]) as f32 * 0.01,
    }))
}

pub async fn read_mug_name<P: Peripheral>(
    peripheral: &P,
) -> Result<String, btleplug::Error> {
    let bytes = read_logged(peripheral, &MUG_NAME_CHAR).await?;
    Ok(String::from_utf8_lossy(&bytes).trim_end_matches('\0').to_string())
}

pub async fn read_mug_color<P: Peripheral>(
    peripheral: &P,
) -> Result<Result<Color, CommandBuilderError>, btleplug::Error> {
    let bytes = read_logged(peripheral, &MUG_COLOR_CHAR).await?;
    if bytes.len() < 4 {
        return Ok(Err(CommandBuilderError::InvalidPayloadLength));
    }
    Ok(Ok(Color {
        red: bytes[0],
        green: bytes[1],
        blue: bytes[2],
        alpha: bytes[3],
    }))
}

fn decode_temperature(bytes: &[u8]) -> Result<f32, CommandBuilderError> {
    if bytes.len() < 2 {
        return Err(CommandBuilderError::InvalidPayloadLength);
    }
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]) as f32 * 0.01)
}
