use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::io::{IsTerminal, Write as _};
use std::process::Command as ProcessCommand;
use std::time::Duration;

use btleplug::api::CentralEvent;
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager, Peripheral};
use clap::{Parser, Subcommand, ValueEnum};
use futures_util::stream::StreamExt;
use log::{debug, info, trace, warn};

use ember2ctl::{
    Color, LiquidLevel, LiquidState, TemperatureUnit, disable_heating, read_battery,
    read_current_temperature_celsius, read_liquid_level, read_liquid_state, read_mug_color,
    read_mug_name, read_target_temperature_celsius, read_temperature_unit, send_command,
    set_mug_color, set_mug_name, set_target_temperature_celsius, set_temperature_unit,
};

const EMBER_NAME: &str = "Ember";

#[derive(Parser, Debug)]
#[command(name = "ember2ctl", about = "Control an Ember² smart mug over BLE")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Bond with the mug via bluetoothctl (one-time, requires pairing mode).
    Pair,
    /// Dump every readable characteristic.
    Info,
    /// Poll info every <secs> seconds; Ctrl-C to stop.
    Watch {
        /// Refresh interval in seconds.
        #[arg(default_value_t = 2)]
        secs: u64,
    },
    /// Print mug name.
    Name,
    /// Set mug name (≤14 ASCII chars, no spaces).
    SetName { name: String },
    /// Current liquid temperature (Celsius).
    Temp,
    /// Target temperature (Celsius, 0 = off).
    Target,
    /// Set target temperature in Celsius (0 or 50.0–100.0).
    SetTarget { celsius: f32 },
    /// Re-send the target temperature every <secs> seconds; Ctrl-C to stop.
    /// Exits automatically once the current temperature reaches the target.
    Retain {
        /// Target temperature in Celsius (0 or 50.0–100.0).
        celsius: f32,
        /// Refresh interval in seconds.
        #[arg(default_value_t = 15)]
        secs: u64,
    },
    /// Disable heating (target = 0).
    Off,
    /// Temperature unit (C or F).
    Unit,
    /// Set display unit.
    SetUnit { unit: CliUnit },
    /// Battery percent, charging state, internal temp.
    Battery,
    /// Liquid level (empty / not empty).
    Level,
    /// Liquid state (empty/filling/cooling/heating/stable).
    State,
    /// LED color as #RRGGBB and alpha byte.
    Color,
    /// Set LED color (alpha defaults to FF).
    SetColor {
        /// Hex color: `rrggbb` or `rrggbbaa`, optional leading `#`.
        #[arg(value_parser = parse_color)]
        color: Color,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum CliUnit {
    #[value(alias = "celsius")]
    C,
    #[value(alias = "fahrenheit")]
    F,
}

impl From<CliUnit> for TemperatureUnit {
    fn from(value: CliUnit) -> Self {
        match value {
            CliUnit::C => TemperatureUnit::Celsius,
            CliUnit::F => TemperatureUnit::Fahrenheit,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();
    debug!("command={:?}", cli.command);

    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    let n_adapters = adapters.len();
    debug!("found {n_adapters} adapter(s)");
    let central = adapters
        .into_iter()
        .next()
        .ok_or("no Bluetooth adapter found")?;

    if matches!(cli.command, Cmd::Pair) {
        return run_pair(&central).await;
    }

    if let Cmd::Watch { secs: 0 } = cli.command {
        return Err("watch interval must be at least 1 second".into());
    }
    if let Cmd::Retain { secs: 0, .. } = cli.command {
        return Err("retain interval must be at least 1 second".into());
    }

    let mug = find_mug(&central).await?;
    if let Some(props) = mug.properties().await? {
        let name = props.local_name.as_deref().unwrap_or("<unnamed>");
        let address = props.address;
        info!("found mug {name:?} at {address}");
    }

    if let Err(e) = connect_with_retry(&mug).await {
        return Err(format!(
            "could not connect to mug: {e}\n\
             Put the mug on its charging coaster (keeps it advertising),\n\
             or hold the button to enter pairing mode."
        )
        .into());
    }
    mug.discover_services().await?;
    debug!("services discovered");

    let result = run_subcommand(&mug, cli.command).await;

    debug!("disconnecting");
    mug.disconnect().await?;
    result
}

async fn run_pair(central: &Adapter) -> Result<(), Box<dyn Error>> {
    eprintln!("Put the mug in pairing mode (hold the button until it flashes), then wait…");
    let mug = find_mug(central).await?;
    let address = mug
        .properties()
        .await?
        .map(|p| p.address)
        .ok_or("mug peripheral has no properties")?;
    let addr = address.to_string();
    info!("pairing {addr} via bluetoothctl");
    bluetoothctl(&["pair", &addr])?;
    bluetoothctl(&["trust", &addr])?;
    info!("paired and trusted — subsequent commands should work without pairing mode");
    Ok(())
}

fn bluetoothctl(args: &[&str]) -> Result<(), Box<dyn Error>> {
    let joined = args.join(" ");
    debug!("$ bluetoothctl {joined}");
    let output = ProcessCommand::new("bluetoothctl")
        .args(args)
        .output()
        .map_err(|e| {
            format!("failed to invoke bluetoothctl (is it installed and on PATH?): {e}")
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        let s = stdout.trim();
        debug!("bluetoothctl stdout: {s}");
    }
    if !output.status.success() {
        let status = output.status;
        let err = stderr.trim();
        return Err(format!("bluetoothctl {joined} failed (status {status}): {err}").into());
    }
    Ok(())
}

async fn connect_with_retry(mug: &Peripheral) -> Result<(), btleplug::Error> {
    if mug.is_connected().await? {
        debug!("already connected");
        return Ok(());
    }

    let attempts: u32 = env::var("EMBER2CTL_CONNECT_RETRIES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let delay = Duration::from_secs(2);

    let mut last_err = None;
    for attempt in 1..=attempts {
        debug!("connect attempt {attempt}/{attempts}");
        match mug.connect().await {
            Ok(()) => {
                debug!("connected on attempt {attempt}");
                return Ok(());
            }
            Err(e) => {
                debug!("connect attempt {attempt} failed: {e}");
                last_err = Some(e);
                if attempt < attempts {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_err.expect("at least one connect attempt was made"))
}

async fn find_mug(central: &Adapter) -> Result<Peripheral, Box<dyn Error>> {
    // The mug only advertises when in pairing mode, but once the OS has bonded
    // it remains in the adapter's known-peripherals list and can be connected
    // to without scanning. Try that path first; fall back to a scan.
    for p in central.peripherals().await? {
        let local_name = p.properties().await?.and_then(|props| props.local_name);
        trace!("known peripheral: {local_name:?}");
        if local_name.as_deref().is_some_and(|n| n.contains(EMBER_NAME)) {
            debug!("using already-known mug");
            return Ok(p);
        }
    }

    info!("no paired Ember mug found; scanning (mug must be in pairing mode)…");
    central.start_scan(ScanFilter::default()).await?;
    let mut events = central.events().await?;
    while let Some(event) = events.next().await {
        if let CentralEvent::DeviceDiscovered(id) = event {
            let discovered: Peripheral = central.peripheral(&id).await?;
            let local_name = discovered.properties().await?.and_then(|p| p.local_name);
            trace!("discovered: {local_name:?}");
            if local_name.as_deref().is_some_and(|n| n.contains(EMBER_NAME)) {
                return Ok(discovered);
            }
        }
    }
    Err("no Ember mug discovered".into())
}

async fn run_subcommand(mug: &Peripheral, cmd: Cmd) -> Result<(), Box<dyn Error>> {
    match cmd {
        Cmd::Pair => unreachable!("pair handled before connection"),
        Cmd::Info => print_info(mug).await,
        Cmd::Watch { secs } => watch_loop(mug, Duration::from_secs(secs)).await,
        Cmd::Name => {
            let name = read_mug_name(mug).await?;
            println!("{name}");
            Ok(())
        }
        Cmd::SetName { name } => {
            let cmd = set_mug_name(&name)?;
            send_command(mug, &cmd).await?;
            Ok(())
        }
        Cmd::Temp => {
            let c = read_current_temperature_celsius(mug).await??;
            println!("{c:.2} C");
            Ok(())
        }
        Cmd::Target => {
            let c = read_target_temperature_celsius(mug).await??;
            println!("{c:.2} C");
            Ok(())
        }
        Cmd::SetTarget { celsius } => {
            let cmd = set_target_temperature_celsius(celsius)?;
            send_command(mug, &cmd).await?;
            Ok(())
        }
        Cmd::Retain { celsius, secs } => {
            retain_loop(mug, celsius, Duration::from_secs(secs)).await
        }
        Cmd::Off => {
            let cmd = disable_heating()?;
            send_command(mug, &cmd).await?;
            Ok(())
        }
        Cmd::Unit => {
            match read_temperature_unit(mug).await?? {
                TemperatureUnit::Celsius => println!("C"),
                TemperatureUnit::Fahrenheit => println!("F"),
            }
            Ok(())
        }
        Cmd::SetUnit { unit } => {
            let cmd = set_temperature_unit(unit.into())?;
            send_command(mug, &cmd).await?;
            Ok(())
        }
        Cmd::Battery => {
            let b = read_battery(mug).await??;
            let pct = b.percent;
            let charging = b.charging;
            let t = b.temperature_c;
            println!("{pct}% (charging: {charging}, internal temp: {t:.2} C)");
            Ok(())
        }
        Cmd::Level => {
            println!("{}", format_level(read_liquid_level(mug).await??));
            Ok(())
        }
        Cmd::State => {
            let s = read_liquid_state(mug).await??;
            println!("{}", format_state(s));
            Ok(())
        }
        Cmd::Color => {
            let c = read_mug_color(mug).await??;
            let (r, g, b, a) = (c.red, c.green, c.blue, c.alpha);
            println!("#{r:02X}{g:02X}{b:02X} alpha={a:02X}");
            Ok(())
        }
        Cmd::SetColor { color } => {
            let cmd = set_mug_color(color)?;
            send_command(mug, &cmd).await?;
            Ok(())
        }
    }
}

async fn watch_loop(mug: &Peripheral, interval: Duration) -> Result<(), Box<dyn Error>> {
    let is_tty = std::io::stdout().is_terminal();
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // Wait for either the next tick or Ctrl-C.
        tokio::select! {
            _ = ticker.tick() => {}
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl-C received, disconnecting…");
                return Ok(());
            }
        }

        // BLE reads can take seconds; Ctrl-C must still interrupt.
        let buffer = tokio::select! {
            res = build_info(mug) => res?,
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl-C received, disconnecting…");
                return Ok(());
            }
        };

        let mut stdout = std::io::stdout().lock();
        if is_tty {
            stdout.write_all(b"\x1b[2J\x1b[H")?;
        }
        stdout.write_all(buffer.as_bytes())?;
        stdout.flush()?;
    }
}

async fn retain_loop(
    mug: &Peripheral,
    celsius: f32,
    interval: Duration,
) -> Result<(), Box<dyn Error>> {
    let cmd = set_target_temperature_celsius(celsius)?;

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl-C received, disconnecting…");
                return Ok(());
            }
        }

        tokio::select! {
            res = send_command(mug, &cmd) => {
                if let Err(e) = res {
                    warn!("retain: set-target failed: {e}");
                } else {
                    debug!("retain: target re-sent ({celsius:.2} C)");
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl-C received, disconnecting…");
                return Ok(());
            }
        }

        if celsius > 0.0 {
            tokio::select! {
                res = read_current_temperature_celsius(mug) => match res {
                    Ok(Ok(current)) => {
                        debug!("retain: current {current:.2} C / target {celsius:.2} C");
                        if current >= celsius {
                            info!(
                                "retain: target reached ({current:.2} C >= {celsius:.2} C), exiting"
                            );
                            return Ok(());
                        }
                    }
                    Ok(Err(e)) => warn!("retain: current temp decode failed: {e}"),
                    Err(e) => warn!("retain: current temp read failed: {e}"),
                },
                _ = tokio::signal::ctrl_c() => {
                    info!("Ctrl-C received, disconnecting…");
                    return Ok(());
                }
            }
        }
    }
}

async fn print_info(mug: &Peripheral) -> Result<(), Box<dyn Error>> {
    print!("{}", build_info(mug).await?);
    Ok(())
}

async fn build_info(mug: &Peripheral) -> Result<String, btleplug::Error> {
    let name = read_mug_name(mug).await?;
    let current = read_current_temperature_celsius(mug).await?;
    let target = read_target_temperature_celsius(mug).await?;
    let unit = read_temperature_unit(mug).await?;
    let battery = read_battery(mug).await?;
    let state = read_liquid_state(mug).await?;
    let level = read_liquid_level(mug).await?;
    let color = read_mug_color(mug).await?;

    let mut out = String::new();
    let _ = writeln!(out, "name:    {name}");
    let _ = match current {
        Ok(c) => writeln!(out, "temp:    {c:.2} C"),
        Err(e) => writeln!(out, "temp:    <{e}>"),
    };
    let _ = match target {
        Ok(0.0) => writeln!(out, "target:  off"),
        Ok(c) => writeln!(out, "target:  {c:.2} C"),
        Err(e) => writeln!(out, "target:  <{e}>"),
    };
    let _ = match unit {
        Ok(TemperatureUnit::Celsius) => writeln!(out, "unit:    C"),
        Ok(TemperatureUnit::Fahrenheit) => writeln!(out, "unit:    F"),
        Err(e) => writeln!(out, "unit:    <{e}>"),
    };
    let _ = match battery {
        Ok(b) => {
            let pct = b.percent;
            let charging = b.charging;
            let t = b.temperature_c;
            writeln!(out, "battery: {pct}% (charging: {charging}, internal: {t:.2} C)")
        }
        Err(e) => writeln!(out, "battery: <{e}>"),
    };
    let _ = match state {
        Ok(s) => writeln!(out, "state:   {}", format_state(s)),
        Err(e) => writeln!(out, "state:   <{e}>"),
    };
    let _ = match level {
        Ok(l) => writeln!(out, "level:   {}", format_level(l)),
        Err(e) => writeln!(out, "level:   <{e}>"),
    };
    let _ = match color {
        Ok(c) => {
            let (r, g, b, a) = (c.red, c.green, c.blue, c.alpha);
            writeln!(out, "color:   #{r:02X}{g:02X}{b:02X} alpha={a:02X}")
        }
        Err(e) => writeln!(out, "color:   <{e}>"),
    };
    Ok(out)
}

fn parse_color(s: &str) -> Result<Color, String> {
    let hex = s.trim_start_matches('#');
    let (rgb_part, alpha) = match hex.len() {
        6 => (hex, 0xFFu8),
        8 => {
            let a = u8::from_str_radix(&hex[6..], 16)
                .map_err(|e| format!("invalid alpha byte: {e}"))?;
            (&hex[..6], a)
        }
        _ => return Err("color must be 6 or 8 hex digits".into()),
    };
    let parse = |part: &str| {
        u8::from_str_radix(part, 16).map_err(|e| format!("invalid hex byte {part:?}: {e}"))
    };
    Ok(Color {
        red: parse(&rgb_part[0..2])?,
        green: parse(&rgb_part[2..4])?,
        blue: parse(&rgb_part[4..6])?,
        alpha,
    })
}

fn format_state(s: LiquidState) -> &'static str {
    match s {
        LiquidState::Empty => "empty",
        LiquidState::Filling => "filling",
        LiquidState::Unknown => "unknown",
        LiquidState::Cooling => "cooling",
        LiquidState::Heating => "heating",
        LiquidState::Stable => "stable",
    }
}

fn format_level(l: LiquidLevel) -> String {
    match l {
        LiquidLevel::Empty => "empty".to_string(),
        LiquidLevel::NotEmpty => "not empty".to_string(),
        LiquidLevel::Intermediate(v) => format!("intermediate ({v})"),
    }
}
