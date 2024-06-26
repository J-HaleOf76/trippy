#![warn(clippy::all, clippy::pedantic, clippy::nursery, rust_2018_idioms)]
#![allow(
    clippy::module_name_repetitions,
    clippy::redundant_field_names,
    clippy::struct_field_names,
    clippy::option_if_let_else,
    clippy::missing_const_for_fn,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::redundant_pub_crate,
    clippy::struct_excessive_bools,
    clippy::cognitive_complexity,
    clippy::option_option
)]
#![deny(unsafe_code)]

use crate::backend::Backend;
use crate::config::{
    LogFormat, LogSpanEvents, Mode, TrippyAction, TrippyConfig, TuiCommandItem, TuiThemeItem,
};
use crate::geoip::GeoIpLookup;
use crate::platform::Platform;
use anyhow::{anyhow, Error};
use backend::trace::Trace;
use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use config::Args;
use frontend::TuiConfig;
use parking_lot::RwLock;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use std::{process, thread};
use strum::VariantNames;
use tracing_chrome::{ChromeLayerBuilder, FlushGuard};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use trippy::dns::{DnsResolver, Resolver};
use trippy::tracing::{
    ChannelConfig, Config, IcmpExtensionParseMode, MultipathStrategy, PlatformImpl, PortDirection,
    Protocol, SocketImpl,
};
use trippy::tracing::{PrivilegeMode, SourceAddr};

mod backend;
mod config;
mod frontend;
mod geoip;
mod platform;
mod report;

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    Platform::acquire_privileges()?;
    let platform = Platform::discover()?;
    match TrippyAction::from(args, &platform)? {
        TrippyAction::Trippy(cfg) => run_trippy(&cfg, &platform)?,
        TrippyAction::PrintTuiThemeItems => print_tui_theme_items(),
        TrippyAction::PrintTuiBindingCommands => print_tui_binding_commands(),
        TrippyAction::PrintConfigTemplate => print_config_template(),
        TrippyAction::PrintManPage => print_man_page()?,
        TrippyAction::PrintShellCompletions(shell) => print_shell_completions(shell)?,
    }
    Ok(())
}

fn run_trippy(cfg: &TrippyConfig, platform: &Platform) -> anyhow::Result<()> {
    let _guard = configure_logging(cfg);
    let resolver = start_dns_resolver(cfg)?;
    let geoip_lookup = create_geoip_lookup(cfg)?;
    let addrs = resolve_targets(cfg, &resolver)?;
    if addrs.is_empty() {
        return Err(anyhow!(
            "failed to find any valid IP addresses for {} for address family {}",
            cfg.targets.join(", "),
            cfg.addr_family,
        ));
    }
    let traces = start_tracers(cfg, &addrs, platform.pid)?;
    Platform::drop_privileges()?;
    run_frontend(cfg, resolver, geoip_lookup, traces)
}

fn print_tui_theme_items() {
    println!("{}", tui_theme_items());
    process::exit(0);
}

fn print_tui_binding_commands() {
    println!("{}", tui_binding_commands());
    process::exit(0);
}

fn print_config_template() {
    println!("{}", include_str!("../trippy-config-sample.toml"));
    process::exit(0);
}

fn print_shell_completions(shell: Shell) -> anyhow::Result<()> {
    println!("{}", shell_completions(shell)?);
    process::exit(0);
}

fn print_man_page() -> anyhow::Result<()> {
    println!("{}", man_page()?);
    process::exit(0);
}

/// Start the DNS resolver.
fn start_dns_resolver(cfg: &TrippyConfig) -> anyhow::Result<DnsResolver> {
    Ok(DnsResolver::start(trippy::dns::Config::new(
        cfg.dns_resolve_method,
        cfg.addr_family,
        cfg.dns_timeout,
    ))?)
}

fn create_geoip_lookup(cfg: &TrippyConfig) -> anyhow::Result<GeoIpLookup> {
    if let Some(path) = cfg.geoip_mmdb_file.as_ref() {
        GeoIpLookup::from_file(path)
    } else {
        Ok(GeoIpLookup::empty())
    }
}

fn configure_logging(cfg: &TrippyConfig) -> Option<FlushGuard> {
    if cfg.verbose {
        let fmt_span = match cfg.log_span_events {
            LogSpanEvents::Off => FmtSpan::NONE,
            LogSpanEvents::Active => FmtSpan::ACTIVE,
            LogSpanEvents::Full => FmtSpan::FULL,
        };
        match cfg.log_format {
            LogFormat::Compact => {
                tracing_subscriber::fmt()
                    .with_span_events(fmt_span)
                    .with_env_filter(&cfg.log_filter)
                    .compact()
                    .init();
            }
            LogFormat::Pretty => {
                tracing_subscriber::fmt()
                    .with_span_events(fmt_span)
                    .with_env_filter(&cfg.log_filter)
                    .pretty()
                    .init();
            }
            LogFormat::Json => {
                tracing_subscriber::fmt()
                    .with_span_events(fmt_span)
                    .with_env_filter(&cfg.log_filter)
                    .json()
                    .init();
            }
            LogFormat::Chrome => {
                let (chrome_layer, guard) = ChromeLayerBuilder::new()
                    .writer(std::io::stdout())
                    .include_args(true)
                    .build();
                tracing_subscriber::registry().with(chrome_layer).init();
                return Some(guard);
            }
        }
    }
    None
}

/// Resolve targets.
fn resolve_targets(cfg: &TrippyConfig, resolver: &DnsResolver) -> anyhow::Result<Vec<TargetInfo>> {
    cfg.targets
        .iter()
        .flat_map(|target| match resolver.lookup(target) {
            Ok(addrs) => addrs
                .into_iter()
                .enumerate()
                .take_while(|(i, _)| if cfg.dns_resolve_all { true } else { *i == 0 })
                .map(|(i, addr)| {
                    let hostname = if cfg.dns_resolve_all {
                        format!("{} [{}]", target, i + 1)
                    } else {
                        target.to_string()
                    };
                    Ok(TargetInfo { hostname, addr })
                })
                .collect::<Vec<_>>()
                .into_iter(),
            Err(e) => {
                vec![Err(anyhow!("failed to resolve target: {} ({})", target, e))].into_iter()
            }
        })
        .collect::<anyhow::Result<Vec<_>>>()
}

/// Start all tracers.
fn start_tracers(
    cfg: &TrippyConfig,
    addrs: &[TargetInfo],
    pid: u16,
) -> anyhow::Result<Vec<TraceInfo>> {
    addrs
        .iter()
        .enumerate()
        .map(|(i, TargetInfo { hostname, addr })| {
            start_tracer(cfg, hostname, *addr, pid + i as u16)
        })
        .collect::<anyhow::Result<Vec<_>>>()
}

/// Start a tracer to a given target.
fn start_tracer(
    cfg: &TrippyConfig,
    target_host: &str,
    target_addr: IpAddr,
    trace_identifier: u16,
) -> Result<TraceInfo, Error> {
    let source_addr = match cfg.source_addr {
        None => SourceAddr::discover::<SocketImpl, PlatformImpl>(
            target_addr,
            cfg.port_direction,
            cfg.interface.as_deref(),
        )?,
        Some(addr) => SourceAddr::validate::<SocketImpl>(addr)?,
    };
    let channel_config = make_channel_config(cfg, source_addr, target_addr);
    let tracer_config = make_tracer_config(cfg, target_addr, trace_identifier)?;
    let backend = Backend::new(
        tracer_config,
        channel_config,
        cfg.tui_max_samples,
        cfg.max_flows(),
    );
    let trace_data = backend.trace();
    thread::Builder::new()
        .name(format!("tracer-{}", tracer_config.trace_identifier.0))
        .spawn(move || backend.start().expect("failed to run tracer backend"))?;
    Ok(make_trace_info(
        cfg,
        trace_data,
        source_addr,
        target_host.to_string(),
        target_addr,
    ))
}

/// Run the TUI, stream or report.
fn run_frontend(
    args: &TrippyConfig,
    resolver: DnsResolver,
    geoip_lookup: GeoIpLookup,
    traces: Vec<TraceInfo>,
) -> anyhow::Result<()> {
    match args.mode {
        Mode::Tui => frontend::run_frontend(traces, make_tui_config(args), resolver, geoip_lookup)?,
        Mode::Stream => report::stream::report(&traces[0], &resolver)?,
        Mode::Csv => report::csv::report(&traces[0], args.report_cycles, &resolver)?,
        Mode::Json => report::json::report(&traces[0], args.report_cycles, &resolver)?,
        Mode::Pretty => report::table::report_pretty(&traces[0], args.report_cycles, &resolver)?,
        Mode::Markdown => report::table::report_md(&traces[0], args.report_cycles, &resolver)?,
        Mode::Dot => report::dot::report(&traces[0], args.report_cycles)?,
        Mode::Flows => report::flows::report(&traces[0], args.report_cycles)?,
        Mode::Silent => report::silent::report(&traces[0], args.report_cycles)?,
    }
    Ok(())
}

/// Make the tracer configuration.
fn make_tracer_config(
    args: &TrippyConfig,
    target_addr: IpAddr,
    trace_identifier: u16,
) -> anyhow::Result<Config> {
    Ok(Config::new(
        target_addr,
        args.protocol,
        args.max_rounds,
        trace_identifier,
        args.first_ttl,
        args.max_ttl,
        args.grace_duration,
        args.max_inflight,
        args.initial_sequence,
        args.multipath_strategy,
        args.port_direction,
        args.min_round_duration,
        args.max_round_duration,
    )?)
}

/// Make the tracer configuration.
fn make_channel_config(
    args: &TrippyConfig,
    source_addr: IpAddr,
    target_addr: IpAddr,
) -> ChannelConfig {
    ChannelConfig::new(
        args.privilege_mode,
        args.protocol,
        source_addr,
        target_addr,
        args.packet_size,
        args.payload_pattern,
        args.initial_sequence,
        args.tos,
        args.icmp_extension_parse_mode,
        args.read_timeout,
        args.min_round_duration,
    )
}

/// Make the per-trace information.
fn make_trace_info(
    args: &TrippyConfig,
    trace_data: Arc<RwLock<Trace>>,
    source_addr: IpAddr,
    target: String,
    target_addr: IpAddr,
) -> TraceInfo {
    TraceInfo::new(
        trace_data,
        source_addr,
        target,
        target_addr,
        args.privilege_mode,
        args.multipath_strategy,
        args.port_direction,
        args.protocol,
        args.first_ttl,
        args.max_ttl,
        args.grace_duration,
        args.min_round_duration,
        args.max_round_duration,
        args.max_inflight,
        args.initial_sequence,
        args.icmp_extension_parse_mode,
        args.read_timeout,
        args.packet_size,
        args.payload_pattern,
        args.interface.clone(),
        args.geoip_mmdb_file.clone(),
        args.dns_resolve_all,
    )
}

/// Make the TUI configuration.
fn make_tui_config(args: &TrippyConfig) -> TuiConfig {
    TuiConfig::new(
        args.tui_refresh_rate,
        args.tui_privacy_max_ttl,
        args.tui_preserve_screen,
        args.tui_address_mode,
        args.dns_lookup_as_info,
        args.tui_as_mode,
        args.tui_icmp_extension_mode,
        args.tui_geoip_mode,
        args.tui_max_addrs,
        args.tui_max_samples,
        args.max_flows(),
        args.tui_theme,
        &args.tui_bindings,
        &args.tui_custom_columns,
    )
}

/// Information about a tracing target.
#[derive(Debug, Clone)]
pub struct TargetInfo {
    pub hostname: String,
    pub addr: IpAddr,
}

/// Information about a `Trace` needed for the Tui, stream and reports.
#[derive(Debug, Clone)]
pub struct TraceInfo {
    pub data: Arc<RwLock<Trace>>,
    pub source_addr: IpAddr,
    pub target_hostname: String,
    pub target_addr: IpAddr,
    pub privilege_mode: PrivilegeMode,
    pub multipath_strategy: MultipathStrategy,
    pub port_direction: PortDirection,
    pub protocol: Protocol,
    pub first_ttl: u8,
    pub max_ttl: u8,
    pub grace_duration: Duration,
    pub min_round_duration: Duration,
    pub max_round_duration: Duration,
    pub max_inflight: u8,
    pub initial_sequence: u16,
    pub icmp_extensions: IcmpExtensionParseMode,
    pub read_timeout: Duration,
    pub packet_size: u16,
    pub payload_pattern: u8,
    pub interface: Option<String>,
    pub geoip_mmdb_file: Option<String>,
    pub dns_resolve_all: bool,
}

impl TraceInfo {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        data: Arc<RwLock<Trace>>,
        source_addr: IpAddr,
        target_hostname: String,
        target_addr: IpAddr,
        privilege_mode: PrivilegeMode,
        multipath_strategy: MultipathStrategy,
        port_direction: PortDirection,
        protocol: Protocol,
        first_ttl: u8,
        max_ttl: u8,
        grace_duration: Duration,
        min_round_duration: Duration,
        max_round_duration: Duration,
        max_inflight: u8,
        initial_sequence: u16,
        icmp_extensions: IcmpExtensionParseMode,
        read_timeout: Duration,
        packet_size: u16,
        payload_pattern: u8,
        interface: Option<String>,
        geoip_mmdb_file: Option<String>,
        dns_resolve_all: bool,
    ) -> Self {
        Self {
            data,
            source_addr,
            target_hostname,
            target_addr,
            privilege_mode,
            multipath_strategy,
            port_direction,
            protocol,
            first_ttl,
            max_ttl,
            grace_duration,
            min_round_duration,
            max_round_duration,
            max_inflight,
            initial_sequence,
            icmp_extensions,
            read_timeout,
            packet_size,
            payload_pattern,
            interface,
            geoip_mmdb_file,
            dns_resolve_all,
        }
    }
}

fn tui_theme_items() -> String {
    format!(
        "TUI theme color items: {}",
        TuiThemeItem::VARIANTS.join(", ")
    )
}

fn tui_binding_commands() -> String {
    format!(
        "TUI binding commands: {}",
        TuiCommandItem::VARIANTS.join(", ")
    )
}

fn shell_completions(shell: Shell) -> anyhow::Result<String> {
    let mut cmd = Args::command();
    let name = cmd.get_name().to_string();
    let mut buffer: Vec<u8> = vec![];
    clap_complete::generate(shell, &mut cmd, name, &mut buffer);
    Ok(String::from_utf8(buffer)?)
}

fn man_page() -> anyhow::Result<String> {
    let cmd = Args::command();
    let mut buffer: Vec<u8> = vec![];
    clap_mangen::Man::new(cmd).render(&mut buffer)?;
    Ok(String::from_utf8(buffer)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(include_str!("../test_resources/tui/tui_theme_items.txt"), &tui_theme_items(); "tui theme items match")]
    #[test_case(include_str!("../test_resources/tui/tui_binding_commands.txt"), &tui_binding_commands(); "tui binding commands match")]
    #[test_case(include_str!("../test_resources/config/completions_bash.txt"), &shell_completions(Shell::Bash).unwrap(); "generate bash shell completions")]
    #[test_case(include_str!("../test_resources/config/completions_elvish.txt"), &shell_completions(Shell::Elvish).unwrap(); "generate elvish shell completions")]
    #[test_case(include_str!("../test_resources/config/completions_fish.txt"), &shell_completions(Shell::Fish).unwrap(); "generate fish shell completions")]
    #[test_case(include_str!("../test_resources/config/completions_powershell.txt"), &shell_completions(Shell::PowerShell).unwrap(); "generate powershell shell completions")]
    #[test_case(include_str!("../test_resources/config/completions_zsh.txt"), &shell_completions(Shell::Zsh).unwrap(); "generate zsh shell completions")]
    #[test_case(include_str!("../test_resources/config/trip.1"), &man_page().unwrap(); "generate man page")]
    fn test_output(expected: &str, actual: &str) {
        if remove_whitespace(actual.to_string()) != remove_whitespace(expected.to_string()) {
            pretty_assertions::assert_eq!(actual, expected);
        }
    }

    fn remove_whitespace(mut s: String) -> String {
        s.retain(|c| !c.is_whitespace());
        s
    }
}
