//! The game inside an app on a phone or tablet (SPEC.md M18). `download` fetches the prepared
//! game from the owner's own Dropbox into the app's private storage, resumably, with the
//! transport passed in: iOS (issue #231) brings a background `URLSession`, Android (issue #230)
//! its own job.

pub mod download;
