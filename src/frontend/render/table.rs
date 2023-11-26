use crate::backend::trace::Hop;
use crate::config::{AddressMode, AsMode, GeoIpMode};
use crate::frontend::config::TuiConfig;
use crate::frontend::theme::Theme;
use crate::frontend::tui_app::TuiApp;
use crate::geoip::{GeoIpCity, GeoIpLookup};
use itertools::Itertools;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Row, Table};
use ratatui::Frame;
use std::net::IpAddr;
use std::rc::Rc;
use trippy::dns::{AsInfo, DnsEntry, DnsResolver, Resolved, Resolver, Unresolved};

/// Render the table of data about the hops.
///
/// For each hop, we show:
///
/// - The time-to-live (indexed from 1) at this hop (`#`)
/// - The host(s) reported at this hop (`Host`)
/// - The packet loss % for all probes at this hop (`Loss%`)
/// - The number of requests sent for all probes at this hop (`Snt`)
/// - The number of replies received for all probes at this hop (`Recv`)
/// - The round-trip time of the most recent probe at this hop (`Last`)
/// - The average round-trip time for all probes at this hop (`Avg`)
/// - The best round-trip time for all probes at this hop (`Best`)
/// - The worst round-trip time for all probes at this hop (`Wrst`)
/// - The standard deviation round-trip time for all probes at this hop (`StDev`)
/// - The status of this hop (`Sts`)
pub fn render(f: &mut Frame<'_>, app: &mut TuiApp, rect: Rect) {
    let header = render_table_header(app.tui_config.theme);
    let selected_style = Style::default().add_modifier(Modifier::REVERSED);
    let rows =
        app.tracer_data().hops(app.selected_flow).iter().map(|hop| {
            render_table_row(app, hop, &app.resolver, &app.geoip_lookup, &app.tui_config)
        });
    let table = Table::new(rows)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(app.tui_config.theme.border_color))
                .title("Hops"),
        )
        .style(
            Style::default()
                .bg(app.tui_config.theme.bg_color)
                .fg(app.tui_config.theme.text_color),
        )
        .highlight_style(selected_style)
        .widths(&TABLE_WIDTH);
    f.render_stateful_widget(table, rect, &mut app.table_state);
}

/// Render the table header.
fn render_table_header(theme: Theme) -> Row<'static> {
    let header_cells = TABLE_HEADER
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(theme.hops_table_header_text_color)));
    Row::new(header_cells)
        .style(Style::default().bg(theme.hops_table_header_bg_color))
        .height(1)
        .bottom_margin(0)
}

/// Render a single row in the table of hops.
fn render_table_row(
    app: &TuiApp,
    hop: &Hop,
    dns: &DnsResolver,
    geoip_lookup: &GeoIpLookup,
    config: &TuiConfig,
) -> Row<'static> {
    let is_selected_hop = app
        .selected_hop()
        .map(|h| h.ttl() == hop.ttl())
        .unwrap_or_default();
    let is_target = app.tracer_data().is_target(hop, app.selected_flow);
    let is_in_round = app.tracer_data().is_in_round(hop, app.selected_flow);
    let ttl_cell = render_ttl_cell(hop);
    let (hostname_cell, row_height) = if is_selected_hop && app.show_hop_details {
        render_hostname_with_details(app, hop, dns, geoip_lookup, config)
    } else {
        render_hostname(app, hop, dns, geoip_lookup)
    };
    let loss_pct_cell = render_loss_pct_cell(hop);
    let total_sent_cell = render_total_sent_cell(hop);
    let total_recv_cell = render_total_recv_cell(hop);
    let last_cell = render_last_cell(hop);
    let avg_cell = render_avg_cell(hop);
    let best_cell = render_best_cell(hop);
    let worst_cell = render_worst_cell(hop);
    let stddev_cell = render_stddev_cell(hop);
    let status_cell = render_status_cell(hop, is_target);
    let cells = [
        ttl_cell,
        hostname_cell,
        loss_pct_cell,
        total_sent_cell,
        total_recv_cell,
        last_cell,
        avg_cell,
        best_cell,
        worst_cell,
        stddev_cell,
        status_cell,
    ];
    let row_color = if is_in_round {
        config.theme.hops_table_row_active_text_color
    } else {
        config.theme.hops_table_row_inactive_text_color
    };
    Row::new(cells)
        .height(row_height)
        .bottom_margin(0)
        .style(Style::default().fg(row_color))
}

fn render_ttl_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(format!("{}", hop.ttl()))
}

fn render_loss_pct_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(format!("{:.1}%", hop.loss_pct()))
}

fn render_total_sent_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(format!("{}", hop.total_sent()))
}

fn render_total_recv_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(format!("{}", hop.total_recv()))
}

fn render_avg_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(if hop.total_recv() > 0 {
        format!("{:.1}", hop.avg_ms())
    } else {
        String::default()
    })
}

fn render_last_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(
        hop.last_ms()
            .map(|last| format!("{last:.1}"))
            .unwrap_or_default(),
    )
}

fn render_best_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(
        hop.best_ms()
            .map(|best| format!("{best:.1}"))
            .unwrap_or_default(),
    )
}

fn render_worst_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(
        hop.worst_ms()
            .map(|worst| format!("{worst:.1}"))
            .unwrap_or_default(),
    )
}

fn render_stddev_cell(hop: &Hop) -> Cell<'static> {
    Cell::from(if hop.total_recv() > 1 {
        format!("{:.1}", hop.stddev_ms())
    } else {
        String::default()
    })
}

fn render_status_cell(hop: &Hop, is_target: bool) -> Cell<'static> {
    let lost = hop.total_sent() - hop.total_recv();
    Cell::from(match (lost, is_target) {
        (lost, target) if target && lost == hop.total_sent() => "🔴",
        (lost, target) if target && lost > 0 => "🟡",
        (lost, target) if !target && lost == hop.total_sent() => "🟤",
        (lost, target) if !target && lost > 0 => "🔵",
        _ => "🟢",
    })
}

/// Render hostname table cell (normal mode).
fn render_hostname(
    app: &TuiApp,
    hop: &Hop,
    dns: &DnsResolver,
    geoip_lookup: &GeoIpLookup,
) -> (Cell<'static>, u16) {
    let (hostname, count) = if hop.total_recv() > 0 {
        if app.hide_private_hops && app.tui_config.privacy_max_ttl >= hop.ttl() {
            (String::from("**Hidden**"), 1)
        } else {
            match app.tui_config.max_addrs {
                None => {
                    let hostnames = hop
                        .addrs_with_counts()
                        .map(|(addr, &freq)| {
                            format_address(addr, freq, hop, dns, geoip_lookup, &app.tui_config)
                        })
                        .join("\n");
                    let count = hop.addr_count().clamp(1, u8::MAX as usize);
                    (hostnames, count as u16)
                }
                Some(max_addr) => {
                    let hostnames = hop
                        .addrs_with_counts()
                        .sorted_unstable_by_key(|(_, &cnt)| cnt)
                        .rev()
                        .take(max_addr as usize)
                        .map(|(addr, &freq)| {
                            format_address(addr, freq, hop, dns, geoip_lookup, &app.tui_config)
                        })
                        .join("\n");
                    let count = hop.addr_count().clamp(1, max_addr as usize);
                    (hostnames, count as u16)
                }
            }
        }
    } else {
        (String::from("No response"), 1)
    };
    (Cell::from(hostname), count)
}

/// Perform a reverse DNS lookup for an address and format the result.
fn format_address(
    addr: &IpAddr,
    freq: usize,
    hop: &Hop,
    dns: &DnsResolver,
    geoip_lookup: &GeoIpLookup,
    config: &TuiConfig,
) -> String {
    let addr_fmt = match config.address_mode {
        AddressMode::IP => addr.to_string(),
        AddressMode::Host => {
            if config.lookup_as_info {
                let entry = dns.lazy_reverse_lookup_with_asinfo(*addr);
                format_dns_entry(entry, true, config.as_mode)
            } else {
                let entry = dns.lazy_reverse_lookup(*addr);
                format_dns_entry(entry, false, config.as_mode)
            }
        }
        AddressMode::Both => {
            let hostname = if config.lookup_as_info {
                let entry = dns.lazy_reverse_lookup_with_asinfo(*addr);
                format_dns_entry(entry, true, config.as_mode)
            } else {
                let entry = dns.lazy_reverse_lookup(*addr);
                format_dns_entry(entry, false, config.as_mode)
            };
            format!("{hostname} ({addr})")
        }
    };
    let geo_fmt = match config.geoip_mode {
        GeoIpMode::Off => None,
        GeoIpMode::Short => geoip_lookup
            .lookup(*addr)
            .unwrap_or_default()
            .map(|geo| geo.short_name()),
        GeoIpMode::Long => geoip_lookup
            .lookup(*addr)
            .unwrap_or_default()
            .map(|geo| geo.long_name()),
        GeoIpMode::Location => geoip_lookup
            .lookup(*addr)
            .unwrap_or_default()
            .map(|geo| geo.location()),
    };
    match geo_fmt {
        Some(geo) if hop.addr_count() > 1 => {
            format!(
                "{} [{}] [{:.1}%]",
                addr_fmt,
                geo,
                (freq as f64 / hop.total_recv() as f64) * 100_f64
            )
        }
        Some(geo) => {
            format!("{addr_fmt} [{geo}]")
        }
        None if hop.addr_count() > 1 => {
            format!(
                "{} [{:.1}%]",
                addr_fmt,
                (freq as f64 / hop.total_recv() as f64) * 100_f64
            )
        }
        None => addr_fmt,
    }
}

/// Format a `DnsEntry` with or without `AS` information (if available)
fn format_dns_entry(dns_entry: DnsEntry, lookup_as_info: bool, as_mode: AsMode) -> String {
    match dns_entry {
        DnsEntry::Resolved(Resolved::Normal(_, hosts)) => hosts.join(" "),
        DnsEntry::Resolved(Resolved::WithAsInfo(_, hosts, asinfo)) => {
            if lookup_as_info && !asinfo.asn.is_empty() {
                format!("{} {}", format_asinfo(&asinfo, as_mode), hosts.join(" "))
            } else {
                hosts.join(" ")
            }
        }
        DnsEntry::NotFound(Unresolved::Normal(ip)) | DnsEntry::Pending(ip) => format!("{ip}"),
        DnsEntry::NotFound(Unresolved::WithAsInfo(ip, asinfo)) => {
            if lookup_as_info && !asinfo.asn.is_empty() {
                format!("{} {}", format_asinfo(&asinfo, as_mode), ip)
            } else {
                format!("{ip}")
            }
        }
        DnsEntry::Failed(ip) => format!("Failed: {ip}"),
        DnsEntry::Timeout(ip) => format!("Timeout: {ip}"),
    }
}

/// Format `AsInfo` based on the `ASDisplayMode`.
fn format_asinfo(asinfo: &AsInfo, as_mode: AsMode) -> String {
    match as_mode {
        AsMode::Asn => format!("AS{}", asinfo.asn),
        AsMode::Prefix => format!("AS{} [{}]", asinfo.asn, asinfo.prefix),
        AsMode::CountryCode => format!("AS{} [{}]", asinfo.asn, asinfo.cc),
        AsMode::Registry => format!("AS{} [{}]", asinfo.asn, asinfo.registry),
        AsMode::Allocated => format!("AS{} [{}]", asinfo.asn, asinfo.allocated),
        AsMode::Name => format!("AS{} [{}]", asinfo.asn, asinfo.name),
    }
}

/// Render hostname table cell (detailed mode).
fn render_hostname_with_details(
    app: &TuiApp,
    hop: &Hop,
    dns: &DnsResolver,
    geoip_lookup: &GeoIpLookup,
    config: &TuiConfig,
) -> (Cell<'static>, u16) {
    let rendered = if hop.total_recv() > 0 {
        if app.hide_private_hops && config.privacy_max_ttl >= hop.ttl() {
            String::from("**Hidden**")
        } else {
            let index = app.selected_hop_address;
            format_details(hop, index, dns, geoip_lookup, config)
        }
    } else {
        String::from("No response")
    };
    (Cell::from(rendered), 6)
}

/// Format hop details.
fn format_details(
    hop: &Hop,
    offset: usize,
    dns: &DnsResolver,
    geoip_lookup: &GeoIpLookup,
    config: &TuiConfig,
) -> String {
    let Some(addr) = hop.addrs().nth(offset) else {
        return format!("Error: no addr for index {offset}");
    };
    let count = hop.addr_count();
    let index = offset + 1;
    let geoip = geoip_lookup.lookup(*addr).unwrap_or_default();
    let dns_entry = if config.lookup_as_info {
        dns.lazy_reverse_lookup_with_asinfo(*addr)
    } else {
        dns.lazy_reverse_lookup(*addr)
    };
    match dns_entry {
        DnsEntry::Pending(addr) => fmt_details_line(addr, index, count, None, None, geoip, config),
        DnsEntry::Resolved(Resolved::WithAsInfo(addr, hosts, asinfo)) => {
            fmt_details_line(addr, index, count, Some(hosts), Some(asinfo), geoip, config)
        }
        DnsEntry::NotFound(Unresolved::WithAsInfo(addr, asinfo)) => fmt_details_line(
            addr,
            index,
            count,
            Some(vec![]),
            Some(asinfo),
            geoip,
            config,
        ),
        DnsEntry::Resolved(Resolved::Normal(addr, hosts)) => {
            fmt_details_line(addr, index, count, Some(hosts), None, geoip, config)
        }
        DnsEntry::NotFound(Unresolved::Normal(addr)) => {
            fmt_details_line(addr, index, count, Some(vec![]), None, geoip, config)
        }
        DnsEntry::Failed(ip) => {
            format!("Failed: {ip}")
        }
        DnsEntry::Timeout(ip) => {
            format!("Timeout: {ip}")
        }
    }
}

/// Format hostname detail lines.
///
/// Format as follows:
///
/// ```
/// 172.217.24.78 [1 of 2]
/// Host: hkg07s50-in-f14.1e100.net
/// AS Name: AS15169 GOOGLE, US
/// AS Info: 142.250.0.0/15 arin 2012-05-24
/// Geo: United States, North America
/// Pos: 37.751, -97.822 (~1000km)
/// ```
fn fmt_details_line(
    addr: IpAddr,
    index: usize,
    count: usize,
    hostnames: Option<Vec<String>>,
    asinfo: Option<AsInfo>,
    geoip: Option<Rc<GeoIpCity>>,
    config: &TuiConfig,
) -> String {
    let as_formatted = match (config.lookup_as_info, asinfo) {
        (false, _) => "AS Name: <not enabled>\nAS Info: <not enabled>".to_string(),
        (true, None) => "AS Name: <awaited>\nAS Info: <awaited>".to_string(),
        (true, Some(info)) if info.asn.is_empty() => {
            "AS Name: <not found>\nAS Info: <not found>".to_string()
        }
        (true, Some(info)) => format!(
            "AS Name: AS{} {}\nAS Info: {} {} {}",
            info.asn, info.name, info.prefix, info.registry, info.allocated
        ),
    };
    let hosts_rendered = if let Some(hosts) = hostnames {
        if hosts.is_empty() {
            "Host: <not found>".to_string()
        } else {
            format!("Host: {}", hosts.join(" "))
        }
    } else {
        "Host: <awaited>".to_string()
    };
    let geoip_formatted = if let Some(geo) = geoip {
        let (lat, long, radius) = geo.coordinates().unwrap_or_default();
        format!(
            "Geo: {}\nPos: {}, {} (~{}km)",
            geo.long_name(),
            lat,
            long,
            radius
        )
    } else {
        "Geo: <not found>\nPos: <not found>".to_string()
    };
    format!("{addr} [{index} of {count}]\n{hosts_rendered}\n{as_formatted}\n{geoip_formatted}")
}

const TABLE_HEADER: [&str; 11] = [
    "#", "Host", "Loss%", "Snt", "Recv", "Last", "Avg", "Best", "Wrst", "StDev", "Sts",
];

const TABLE_WIDTH: [Constraint; 11] = [
    Constraint::Percentage(3),
    Constraint::Percentage(42),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
    Constraint::Percentage(5),
];
