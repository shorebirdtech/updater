#[cfg(any(target_os = "android", test))]
pub mod android;
#[cfg(any(target_os = "ios", test))]
pub mod ios;
#[cfg(not(any(target_os = "android", target_os = "ios", test)))]
pub mod unknown;

#[cfg(any(target_os = "android", test))]
pub use android::*;
#[cfg(target_os = "ios")]
pub use ios::*;
#[cfg(not(any(target_os = "android", target_os = "ios", test)))]
pub use unknown::*;
