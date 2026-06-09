//! Integration with the legacy ETIT ASP.NET system.
//!
//! Submodules:
//! - [`auth`]      — login flow, ASP.NET WebForms scraping, cross-domain
//!                   cookie capture, session epoch
//! - [`client`]    — auth-aware reqwest wrapper used by every data call
//! - [`vehicles`]  — `/WebPages/maps.aspx/GetTransportersPage`
//! - [`live`]      — `/WebPages/GetTransportersSpeedStatus.aspx`
//! - [`history`]   — `/WebPages/GetAllHistoryData.aspx`
//! - [`users`]     — `/WebPages/Users/UserList.aspx` (login host; unblock)
//! - [`parsers`]   — string-format helpers shared by the above

pub mod auth;
pub mod client;
pub mod history;
pub mod live;
pub mod parsers;
pub mod users;
pub mod vehicles;

pub use client::EtitClient;
