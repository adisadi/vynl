//! Deterministic `insta` UI layout snapshot tests covering all 4 responsive
//! breakpoints, Zen Now Playing mode, and floating modals (including
//! Modal-Aware Image Occlusion verification).

use vynl_core::keymap::{KeyCode, KeyEvent};
use vynl_core::state::{ActiveModal, AppEvent, reduce};
use vynl_images::ImageProtocol;
use vynl_tui::app::VynlApp;

#[test]
fn snapshot_wide_layout_home() {
    let mut app = VynlApp::new_demo(ImageProtocol::Halfblocks);
    let frame = app.render_snapshot(150, 36);
    insta::assert_snapshot!(frame.to_plain_text());
}

#[test]
fn snapshot_medium_layout_home() {
    let mut app = VynlApp::new_demo(ImageProtocol::Halfblocks);
    let frame = app.render_snapshot(110, 30);
    insta::assert_snapshot!(frame.to_plain_text());
}

#[test]
fn snapshot_compact_layout_home() {
    let mut app = VynlApp::new_demo(ImageProtocol::Halfblocks);
    let frame = app.render_snapshot(72, 24);
    insta::assert_snapshot!(frame.to_plain_text());
}

#[test]
fn snapshot_mini_player_layout() {
    let mut app = VynlApp::new_demo(ImageProtocol::Halfblocks);
    let frame = app.render_snapshot(50, 12);
    insta::assert_snapshot!(frame.to_plain_text());
}

#[test]
fn snapshot_zen_now_playing() {
    let mut app = VynlApp::new_demo(ImageProtocol::Halfblocks);
    app.handle_key(KeyEvent::plain(KeyCode::Char('z')));
    app.on_tick(250);
    let frame = app.render_snapshot(140, 34);
    insta::assert_snapshot!(frame.to_plain_text());
}

#[test]
fn snapshot_command_palette_modal_with_occlusion() {
    let mut app = VynlApp::new_demo(ImageProtocol::Halfblocks);
    app.handle_key(KeyEvent::ctrl('p'));
    let frame = app.render_snapshot(140, 34);
    insta::assert_snapshot!(frame.to_plain_text());
}

#[test]
fn snapshot_device_picker_modal() {
    let mut app = VynlApp::new_demo(ImageProtocol::Halfblocks);
    app.handle_key(KeyEvent::plain(KeyCode::Char('d')));
    let frame = app.render_snapshot(140, 34);
    insta::assert_snapshot!(frame.to_plain_text());
}

#[test]
fn snapshot_setup_wizard_modal() {
    let mut app = VynlApp::new_demo(ImageProtocol::Halfblocks);
    let _ = reduce(
        &mut app.state,
        AppEvent::OpenModal(ActiveModal::SetupWizard {
            client_id_input: String::from("demo-client-id"),
        }),
    );
    let frame = app.render_snapshot(140, 34);
    insta::assert_snapshot!(frame.to_plain_text());
}

#[test]
fn snapshot_help_modal() {
    let mut app = VynlApp::new_demo(ImageProtocol::Halfblocks);
    app.handle_key(KeyEvent::plain(KeyCode::Char('?')));
    let frame = app.render_snapshot(140, 34);
    insta::assert_snapshot!(frame.to_plain_text());
}
