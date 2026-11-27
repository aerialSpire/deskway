//! Wire types for the channels Deskway's Rust programs speak with the server and with each
//! other, as defined in `proto/messages.json`, the strings either side signs (`challenge`), and
//! the primitives those need (`crypto`).
//!
//! Nothing here is generated: the types are written by hand and the contract tests
//! (`tests.rs`) check them against the definition, both ways, so neither can drift.
//! Each enum is declared through `wire_enum!`, which also produces the table of wire names the
//! tests compare; the table cannot disagree with the enum because both come from one list.
//! Those tables, and the readers of the definition and of `proto/vectors.json` (`contract`),
//! are built for this crate's tests and, with the `contract` feature, for the contract tests of
//! the crates that build on it (the host checks its agent pipe and its pairing against them).

pub mod browse;
pub mod challenge;
pub mod control;
pub mod crypto;
pub mod file;
pub mod file_transfer;
pub mod mcp;
pub mod moves;
pub mod signal;
pub mod terminal;
pub mod tools;
pub mod types;

#[cfg(any(test, feature = "contract"))]
pub mod contract;
#[cfg(test)]
mod tests;

/// The protocol version this build speaks. Version 1 is everything before `hello` existed.
pub const PROTOCOL_VERSION: u32 = 5;

/// The oldest host protocol supported by this release.
#[cfg(any(test, feature = "contract"))]
pub const MIN_VERSION: u32 = PROTOCOL_VERSION - 1;

/// Shared maximum frame size for terminal and file data.
pub const DATA_CHUNK: usize = 16 * 1024;

/// Capability names a host can announce in its `hello`. Which ones a given host announces
/// depends on its platform and settings (a view-only host leaves out `input`).
pub mod caps {
    pub const INPUT: &str = "input";
    pub const FILES: &str = "files";
    pub const CLIPBOARD: &str = "clipboard";
    pub const CURSOR: &str = "cursor";
    pub const DISPLAYS: &str = "displays";
    pub const START_DISPLAY: &str = "start-display";
    pub const RELMOUSE: &str = "relmouse";
    pub const LAYOUT: &str = "layout";
    pub const CHARS: &str = "chars";
    pub const CAPTURE: &str = "capture";
    pub const DESK: &str = "desk";
    pub const TERMINAL: &str = "terminal";
    pub const TERMINAL_KEEP: &str = "terminal-keep";
    pub const BROWSE: &str = "browse";
    pub const FILE_TRANSFER: &str = "file-transfer";
    /// It takes AI clients (docs/architecture.md, "AI control").
    pub const AI: &str = "ai";
    pub const RELAY_RTT: &str = "relay-rtt";
    pub const VITALS: &str = "vitals";
    #[cfg(any(test, feature = "contract"))]
    pub const ALL: &[&str] = &[INPUT, FILES, CLIPBOARD, CURSOR, DISPLAYS, START_DISPLAY, RELMOUSE, LAYOUT, CHARS, CAPTURE, DESK, TERMINAL, TERMINAL_KEEP, BROWSE, FILE_TRANSFER, AI, RELAY_RTT, VITALS];
}

/// Declares one channel's messages as an internally tagged serde enum and its table of
/// `(type, fields)` wire names. Optional fields are `Option` with
/// `#[serde(skip_serializing_if = "Option::is_none")]`, so absent stays absent on the wire.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident (tag $tag:tt) {
            $(
                $(#[$vmeta:meta])*
                $wire:literal => $variant:ident { $( $(#[$fmeta:meta])* $fwire:literal $field:ident : $fty:ty ),* $(,)? }
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(tag = $tag)]
        pub enum $name {
            $(
                $(#[$vmeta])*
                #[serde(rename = $wire)]
                $variant { $( $(#[$fmeta])* #[serde(rename = $fwire)] $field: $fty ),* }
            ),*
        }

        #[cfg(any(test, feature = "contract"))]
        impl $name {
            /// Every message of this enum: its type and its field names, as on the wire.
            pub const TYPES: &'static [(&'static str, &'static [&'static str])] = &[ $( ($wire, &[ $($fwire),* ]) ),* ];

            /// The discriminator value of this message.
            pub fn kind(&self) -> &'static str {
                match self { $( Self::$variant { .. } => $wire ),* }
            }
        }
    };
}

/// Declares a struct used inside messages, with its wire field names for the contract tests.
macro_rules! wire_struct {
    (
        $(#[$meta:meta])*
        pub struct $name:ident { $( $(#[$fmeta:meta])* $fwire:literal $field:ident : $fty:ty ),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct $name { $( $(#[$fmeta])* #[serde(rename = $fwire)] pub $field: $fty ),* }

        #[cfg(any(test, feature = "contract"))]
        impl $name {
            pub const FIELDS: &'static [&'static str] = &[ $($fwire),* ];
        }
    };
}

/// Declares a string enum (a field restricted to a set of values).
macro_rules! wire_values {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $( $wire:literal => $variant:ident ),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        pub enum $name { $( #[serde(rename = $wire)] $variant ),* }

        impl $name {
            /// The value as the wire writes it.
            pub fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $wire ),* }
            }
        }

        #[cfg(any(test, feature = "contract"))]
        impl $name {
            pub const VALUES: &'static [&'static str] = &[ $($wire),* ];
        }
    };
}

pub(crate) use {wire_enum, wire_struct, wire_values};
