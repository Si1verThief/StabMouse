//! Screen enumeration over `wl_output`.
//!
//! This is protocol, not desktop: any Wayland compositor advertises its outputs this way, so
//! nothing here is KDE-specific.
//!
//! **Version 4 is requested for a reason.** `wl_output` only gained the `name` event — the
//! connector name, `DP-2` — in version 4. Without it an output can only be identified by its
//! make and model strings, which are neither unique nor what a compositor uses in its own
//! settings, so a tablet could not be mapped to one by name.

use crate::{Error, Output, Result};
use std::collections::HashMap;
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

#[derive(Default)]
struct Collector {
    /// Keyed by the protocol object id, since a name arrives in a different event from the
    /// geometry and they have to be reassembled.
    pending: HashMap<u32, Partial>,
}

#[derive(Default, Clone)]
struct Partial {
    name: Option<String>,
    description: Option<String>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    /// Set by `done`, which the compositor sends once an output's events are complete.
    complete: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Collector {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "wl_output" {
                // Cap at 4: asking for more than the compositor offers is a protocol error, and
                // nothing beyond 4 is needed here.
                let bind_version = version.min(4);
                let output = registry.bind::<wl_output::WlOutput, _, _>(name, bind_version, qh, ());
                state.pending.insert(output.id().protocol_id(), Partial::default());
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Collector {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let entry = state.pending.entry(output.id().protocol_id()).or_default();
        match event {
            wl_output::Event::Geometry { x, y, .. } => {
                entry.x = x;
                entry.y = y;
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                // Several modes are advertised; only the current one describes the layout.
                if flags
                    .into_result()
                    .is_ok_and(|f| f.contains(wl_output::Mode::Current))
                {
                    entry.width = width;
                    entry.height = height;
                }
            }
            wl_output::Event::Name { name } => entry.name = Some(name),
            wl_output::Event::Description { description } => {
                entry.description = Some(description)
            }
            wl_output::Event::Done => entry.complete = true,
            _ => {}
        }
    }
}

/// Every screen the compositor currently has, in no particular order.
pub fn outputs() -> Result<Vec<Output>> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Err(Error::NoWayland);
    }

    let conn = Connection::connect_to_env().map_err(|e| Error::Wayland(e.to_string()))?;
    let display = conn.display();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    display.get_registry(&qh, ());

    let mut state = Collector::default();

    // Two rounds, not one. The first delivers the registry globals; binding an output during it
    // is what causes its own events to be sent, and those only arrive on the next round.
    for _ in 0..2 {
        queue
            .roundtrip(&mut state)
            .map_err(|e| Error::Wayland(e.to_string()))?;
    }

    let mut found: Vec<Output> = state
        .pending
        .into_values()
        // An output missing its `done`, its name, or its size is one the compositor has not
        // finished describing. Reporting a half-built screen would place a tablet on a
        // rectangle that does not exist.
        .filter(|p| p.complete && p.width > 0 && p.height > 0)
        .filter_map(|p| {
            Some(Output {
                name: p.name?,
                description: p.description,
                x: p.x,
                y: p.y,
                width: p.width,
                height: p.height,
            })
        })
        .collect();

    // Left-to-right, then top-to-bottom: a stable order so anything derived from it — config
    // written out, screens listed to the user — does not reshuffle between runs.
    found.sort_by_key(|o| (o.x, o.y, o.name.clone()));
    Ok(found)
}
