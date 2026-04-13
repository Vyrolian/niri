use crate::layout::ActivateWindow;
use crate::niri::RedrawState;
use crate::tests::client::ClientId;
use crate::tests::Fixture;
use smithay::reexports::wayland_protocols::wp::fifo::v1::client::{
    wp_fifo_manager_v1::WpFifoManagerV1, wp_fifo_v1::WpFifoV1,
};
use smithay::wayland::compositor::{with_states, with_surface_tree_downward, TraversalAction};
use smithay::wayland::fifo::FifoBarrierCachedState;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::{protocol::wl_surface::WlSurface, Connection, Dispatch, QueueHandle};

// --- Client Dispatch Implementations (Only the ones Niri doesn't provide) ---

impl Dispatch<WpFifoManagerV1, ()> for crate::tests::client::State {
    fn event(
        _: &mut Self,
        _: &WpFifoManagerV1,
        _: <WpFifoManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<WpFifoV1, ()> for crate::tests::client::State {
    fn event(
        _: &mut Self,
        _: &WpFifoV1,
        _: <WpFifoV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<WlSubcompositor, ()> for crate::tests::client::State {
    fn event(
        _: &mut Self,
        _: &WlSubcompositor,
        _: <WlSubcompositor as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<WlSubsurface, ()> for crate::tests::client::State {
    fn event(
        _: &mut Self,
        _: &WlSubsurface,
        _: <WlSubsurface as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// --- Client Extensions ---

impl crate::tests::client::Client {
    pub fn bind_global<P: wayland_client::Proxy + 'static>(&self, version: u32) -> P
    where
        crate::tests::client::State: Dispatch<P, ()>,
    {
        let interface = P::interface().name;
        let global = self
            .state
            .globals
            .iter()
            .find(|g| g.interface == interface)
            .expect(&format!("Global {} not found", interface));
        let registry = self.connection.display().get_registry(&self.qh, ());
        registry.bind(global.name, version, &self.qh, ())
    }
}

// --- Helper: setup a mapped window ---

fn setup_mapped_window(f: &mut Fixture) -> (ClientId, WlSurface, WpFifoManagerV1) {
    let id = f.add_client();
    let surface = {
        let window = f.client(id).create_window();
        let surface = window.surface.clone();
        window.commit();
        surface
    };
    f.roundtrip(id);
    {
        let window = f.client(id).window(&surface);
        window.attach_new_buffer();
        window.ack_last_and_commit();
    }
    f.double_roundtrip(id);
    let fifo_manager: WpFifoManagerV1 = f.client(id).bind_global(1);
    (id, surface, fifo_manager)
}

// --- Tests ---

#[test]
fn test_fifo_complete_lifecycle() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let (id, surface, fifo_manager) = setup_mapped_window(&mut f);
    let fifo = fifo_manager.get_fifo(&surface, &f.client(id).qh, ());
    let output = f.state.server.state.niri.sorted_outputs[0].clone();

    f.state
        .server
        .state
        .niri
        .output_state
        .get_mut(&output)
        .unwrap()
        .redraw_state = RedrawState::WaitingForVBlank {
        redraw_needed: false,
    };

    fifo.set_barrier();
    f.client(id).window(&surface).commit();
    f.roundtrip(id);

    assert!(f.state.server.state.niri.output_has_fifo_waiters(&output));

    f.state
        .server
        .state
        .niri
        .output_state
        .get_mut(&output)
        .unwrap()
        .redraw_state = RedrawState::Idle;
    f.state.server.state.refresh_and_flush_clients();

    let server_surface = f
        .state
        .server
        .state
        .niri
        .layout
        .windows_for_output(&output)
        .next()
        .unwrap()
        .window
        .toplevel()
        .unwrap()
        .wl_surface()
        .clone();
    with_states(&server_surface, |states| {
        let mut fifo_state = states.cached_state.get::<FifoBarrierCachedState>();
        assert!(fifo_state.current().barrier.is_none());
    });
}

#[test]
fn test_fifo_unmapped_window_deadlock() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let output = f.state.server.state.niri.sorted_outputs[0].clone();

    let window_handle = f.client(id).create_window();
    let client_surface = window_handle.surface.clone();
    let fifo_manager: WpFifoManagerV1 = f.client(id).bind_global(1);
    let fifo = fifo_manager.get_fifo(&client_surface, &f.client(id).qh, ());

    fifo.set_barrier();
    f.client(id).window(&client_surface).commit();
    f.roundtrip(id);

    assert!(
        f.state.server.state.niri.output_has_fifo_waiters(&output),
        "Unmapped window barrier not detected"
    );

    f.state.server.state.signal_fifo(&output);

    let server_surface = f
        .state
        .server
        .state
        .niri
        .unmapped_windows
        .keys()
        .next()
        .unwrap()
        .clone();
    with_states(&server_surface, |states| {
        let mut fifo_state = states.cached_state.get::<FifoBarrierCachedState>();
        assert!(
            fifo_state.current().barrier.is_none(),
            "Unmapped barrier not cleared"
        );
    });
}

#[test]
fn test_fifo_unmapped_frame_callbacks() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let window_handle = f.client(id).create_window();
    let client_surface = window_handle.surface.clone();
    let sync_data = f.client(id).send_sync();
    let _frame_callback = client_surface.frame(&f.client(id).qh, sync_data);

    let fifo_manager: WpFifoManagerV1 = f.client(id).bind_global(1);
    let fifo = fifo_manager.get_fifo(&client_surface, &f.client(id).qh, ());

    fifo.set_barrier();
    f.client(id).window(&client_surface).commit();
    f.roundtrip(id);

    f.state.server.state.refresh_and_flush_clients();

    let server_surface = f
        .state
        .server
        .state
        .niri
        .unmapped_windows
        .keys()
        .next()
        .unwrap()
        .clone();
    with_states(&server_surface, |states| {
        let mut fifo_state = states.cached_state.get::<FifoBarrierCachedState>();
        assert!(
            fifo_state.current().barrier.is_none(),
            "FIFO barrier was not consumed by the redraw loop"
        );
    });

    f.roundtrip(id);
}

#[test]
fn test_fifo_subsurface_barrier() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let output = f.state.server.state.niri.sorted_outputs[0].clone();

    let subcompositor: WlSubcompositor = f.client(id).bind_global(1);
    let compositor: WlCompositor = f.client(id).bind_global(5);

    let parent_handle = f.client(id).create_window();
    let parent_surface = parent_handle.surface.clone();
    let child_surface = compositor.create_surface(&f.client(id).qh, ());
    let _subsurface =
        subcompositor.get_subsurface(&child_surface, &parent_surface, &f.client(id).qh, ());

    let fifo_manager: WpFifoManagerV1 = f.client(id).bind_global(1);
    let fifo = fifo_manager.get_fifo(&child_surface, &f.client(id).qh, ());

    fifo.set_barrier();
    f.client(id).window(&parent_surface).commit();
    f.roundtrip(id);

    // SHOULD FAIL if Niri doesn't walk the subsurface tree
    assert!(
        f.state.server.state.niri.output_has_fifo_waiters(&output),
        "Subsurface barrier not detected"
    );

    f.state.server.state.signal_fifo(&output);

    let server_parent = f
        .state
        .server
        .state
        .niri
        .unmapped_windows
        .keys()
        .next()
        .unwrap();
    let mut child_found_and_cleared = false;
    with_surface_tree_downward(
        server_parent,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_, states, _| {
            let mut fifo_state = states.cached_state.get::<FifoBarrierCachedState>();
            if fifo_state.current().barrier.is_none() {
                child_found_and_cleared = true;
            }
        },
        |_, _, _| true,
    );
    assert!(child_found_and_cleared, "Subsurface barrier not cleared");
}
#[test]
fn test_fifo_mapped_popup_barrier() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let (id, parent_surface, fifo_manager) = setup_mapped_window(&mut f);
    let output = f.state.server.state.niri.sorted_outputs[0].clone();

    // 1. Create a popup.
    let popup_handle = f.client(id).create_window(); // Using window helper
    let popup_surface = popup_handle.surface.clone();

    // 2. Set barrier on the popup.
    let fifo = fifo_manager.get_fifo(&popup_surface, &f.client(id).qh, ());
    fifo.set_barrier();

    f.client(id).window(&popup_surface).commit();
    f.roundtrip(id);

    // 3. Verify detection.
    assert!(
        f.state.server.state.niri.output_has_fifo_waiters(&output),
        "Niri failed to detect a FIFO barrier on an xdg_popup"
    );
}
#[test]
fn test_fifo_unmap_zombie_simulation() {
    // SIMULATES: "Won't open a second time"
    // Scenario: Client is closing. It attaches nil (unmap) and sets a FIFO barrier.
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let (id, client_surface, fifo_manager) = setup_mapped_window(&mut f);
    let output = f.state.server.state.niri.sorted_outputs[0].clone();

    // 1. Client prepares to exit: attach(nil) + set_barrier
    let fifo = fifo_manager.get_fifo(&client_surface, &f.client(id).qh, ());
    fifo.set_barrier();

    // wayland-client call to unmap
    client_surface.attach(None, 0, 0);
    client_surface.commit();
    f.roundtrip(id);

    // 2. Verify the window is now in unmapped_windows
    assert!(!f.state.server.state.niri.unmapped_windows.is_empty());

    // 3. Get the server-side surface handle from the map keys
    // (This avoids the Borrow<WlSurface> trait error)
    let server_surface = f
        .state
        .server
        .state
        .niri
        .unmapped_windows
        .keys()
        .next()
        .unwrap()
        .clone();

    // 4. Trigger the refresh loop
    f.state.server.state.refresh_and_flush_clients();

    // 5. ASSERTION: The barrier must be cleared.
    // If Niri doesn't signal unmapped windows, this will fail.
    with_states(&server_surface, |states| {
        let mut fifo_state = states.cached_state.get::<FifoBarrierCachedState>();
        assert!(
            fifo_state.current().barrier.is_none(),
            "FAILURE: FIFO barrier on unmapping window was NOT cleared."
        );
    });
}

#[test]
fn test_fifo_freeze_on_active_output_simulation() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let output = f.state.server.state.niri.sorted_outputs[0].clone();

    f.state.server.state.niri.layout.focus_output(&output);

    let compositor: WlCompositor = f.client(id).bind_global(5);
    let client_surface = compositor.create_surface(&f.client(id).qh, ());
    let fifo_manager: WpFifoManagerV1 = f.client(id).bind_global(1);
    let fifo = fifo_manager.get_fifo(&client_surface, &f.client(id).qh, ());

    fifo.set_barrier();
    client_surface.commit();
    f.roundtrip(id);

    // If this returns false, Niri skips the redraw and the client freezes.
    assert!(
        f.state.server.state.niri.output_has_fifo_waiters(&output),
        "FAILURE: Active output ignored unmapped FIFO waiter."
    );
}
use smithay::reexports::wayland_protocols::wp::commit_timing::v1::client::{
    wp_commit_timer_v1::WpCommitTimerV1, wp_commit_timing_manager_v1::WpCommitTimingManagerV1,
};

impl Dispatch<WpCommitTimingManagerV1, ()> for crate::tests::client::State {
    fn event(
        _: &mut Self,
        _: &WpCommitTimingManagerV1,
        _: <WpCommitTimingManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<WpCommitTimerV1, ()> for crate::tests::client::State {
    fn event(
        _: &mut Self,
        _: &WpCommitTimerV1,
        _: <WpCommitTimerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
#[test]
fn test_kde_polkit_closing_deadlock() {
    // SIMULATES: "Won't open a second time"
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let (id, client_surface, fifo_manager) = setup_mapped_window(&mut f);
    let output = f.state.server.state.niri.sorted_outputs[0].clone();

    // 1. Setup both FIFO and Commit Timing
    let timing_mgr: WpCommitTimingManagerV1 = f.client(id).bind_global(1);
    let _timer = timing_mgr.get_timer(&client_surface, &f.client(id).qh, ());
    let fifo = fifo_manager.get_fifo(&client_surface, &f.client(id).qh, ());

    // 2. Client begins unmap sequence (attach nil)
    fifo.set_barrier();
    client_surface.attach(None, 0, 0);
    client_surface.commit();
    f.roundtrip(id);

    // 3. Get the server-side surface handle from the unmapped map
    let server_surface = f
        .state
        .server
        .state
        .niri
        .unmapped_windows
        .keys()
        .next()
        .unwrap()
        .clone();

    // 4. Trigger signaling
    f.state.server.state.refresh_and_flush_clients();

    // 5. ASSERTION: Barrier must be cleared even on an unmapped surface
    with_states(&server_surface, |states| {
        let mut fifo_state = states.cached_state.get::<FifoBarrierCachedState>();
        assert!(
            fifo_state.current().barrier.is_none(),
            "FIFO barrier leaked on unmapped surface"
        );
    });
}

#[test]
fn test_fifo_transaction_stall_simulation() {
    // SIMULATES: "Doesn't rerender until resize"
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let output = f.state.server.state.niri.sorted_outputs[0].clone();

    let window_handle = f.client(id).create_window();
    let client_surface = window_handle.surface.clone();

    // Bind protocols
    let fifo_manager: WpFifoManagerV1 = f.client(id).bind_global(1);
    let fifo = fifo_manager.get_fifo(&client_surface, &f.client(id).qh, ());

    // Set barrier and commit while unmapped
    fifo.set_barrier();
    client_surface.commit();
    f.roundtrip(id);

    // Get server handle from unmapped_windows keys
    let server_surface = f
        .state
        .server
        .state
        .niri
        .unmapped_windows
        .keys()
        .next()
        .unwrap()
        .clone();

    // ASSERTION: output_has_fifo_waiters must see the barrier on the server_surface
    // even though it isn't in the layout yet.
    // If this is false, Niri skips the redraw loop and signals never fire.
    let has_waiters = f.state.server.state.niri.output_has_fifo_waiters(&output);
    assert!(
        has_waiters,
        "Niri ignored a FIFO barrier on an unmapped window, causing a stall"
    );

    // Clean up
    f.state.server.state.signal_fifo(&output);
    with_states(&server_surface, |states| {
        let mut fifo_state = states.cached_state.get::<FifoBarrierCachedState>();
        assert!(fifo_state.current().barrier.is_none());
    });
}
#[test]
fn test_fifo_zombie_on_immediate_destroy() {
    // SIMULATES: "Won't open a second time"
    // Logs show client attaches nil, commits, and then destroys resources.
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let (id, client_surface, fifo_manager) = setup_mapped_window(&mut f);

    // 1. Get the server-side surface handle (server-side WlSurface)
    let server_surface = f
        .state
        .server
        .state
        .niri
        .layout
        .windows()
        .next()
        .map(|(_, m)| m.toplevel().wl_surface().clone()) // Removed .unwrap() here
        .unwrap();

    // 2. Set barrier and commit
    let fifo = fifo_manager.get_fifo(&client_surface, &f.client(id).qh, ());
    fifo.set_barrier();
    client_surface.commit();

    // 3. Client immediately destroys the surface
    // This replicates the closing sequence of KDE Polkit
    client_surface.destroy();
    f.double_roundtrip(id);

    // 4. ASSERTION: Barrier must be cleared during destruction.
    // If Niri's destruction handler doesn't signal FIFO, the process stays a zombie.
    with_states(&server_surface, |states| {
        let mut fifo_state = states.cached_state.get::<FifoBarrierCachedState>();
        assert!(
            fifo_state.current().barrier.is_none(),
            "FAILURE: FIFO barrier was NOT cleared during surface destruction. Client is a zombie."
        );
    });
}
#[test]
fn test_fifo_commit_timing_interlock_stall() {
    // SIMULATES: "Doesn't rerender until resize"
    // Scenario: Client uses both Commit Timing and FIFO.
    // Niri signals Commit Timing but doesn't trigger a Redraw (and thus signal_fifo)
    // because the window is in a state Niri considers "invisible" or "idle".
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let (id, client_surface, fifo_manager) = setup_mapped_window(&mut f);
    let output = f.state.server.state.niri.sorted_outputs[0].clone();

    // 1. Setup both
    let timing_mgr: WpCommitTimingManagerV1 = f.client(id).bind_global(1);
    let _timer = timing_mgr.get_timer(&client_surface, &f.client(id).qh, ());
    let fifo = fifo_manager.get_fifo(&client_surface, &f.client(id).qh, ());

    // 2. Commit with FIFO barrier
    fifo.set_barrier();
    client_surface.commit();
    f.roundtrip(id);

    // 3. Force the window into an "idle" state (e.g. no damage)
    // In this state, Niri might decide not to redraw the output.

    // 4. Trigger the refresh loop
    f.state.server.state.refresh_and_flush_clients();

    // 5. ASSERTION: output_has_fifo_waiters must return true to FORCE a redraw
    // even if there is no surface damage, specifically to clear the FIFO barrier.
    assert!(
        f.state.server.state.niri.output_has_fifo_waiters(&output),
        "FAILURE: Niri skipped redraw despite a pending FIFO barrier."
    );
}
