//! Platform-independent Legato logic.
//!
//! Nothing in this crate touches the OS or the network, so all of it is deterministic
//! and unit-testable. OS backends feed events in and carry out the returned actions.

#![forbid(unsafe_code)]

pub mod activity;
pub mod controller;
pub mod keymap;
pub mod layout;
pub mod receiver;

pub use activity::{ActivityFilter, LocalInput};
pub use controller::{Action, CaptureCommand, Controller, ControllerConfig, Event, Verdict};
pub use keymap::KeyRemap;
pub use layout::{Align, Layout, Machine, MachineId, Side};
pub use receiver::{Inject, Receiver, ReceiverConfig};
