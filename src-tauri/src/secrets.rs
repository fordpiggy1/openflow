use std::collections::HashMap;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::io::Write;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::{Component, PathBuf};
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::process::{Command, Stdio};
use std::sync::Mutex;

#[cfg(any(target_os = "macos", target_os = "linux"))]
const SERVICE: &str = "io.laisy.openflow";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Stores API credentials using the host OS credential vault. Secrets are never
/// written to SQLite or application logs.
pub struct SecretStore {
    cache: Mutex<HashMap<String, Option<String>>>,
    #[cfg(target_os = "windows")]
    app_dir: PathBuf,
}

impl SecretStore {
    pub fn new(app_dir: PathBuf) -> Self {
        #[cfg(target_os = "windows")]
        {
            Self {
                cache: Mutex::new(HashMap::new()),
                app_dir,
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = app_dir;
            Self {
                cache: Mutex::new(HashMap::new()),
            }
        }
    }

    pub fn get(&self, account: &str) -> Result<Option<String>, String> {
        validate_account(account)?;
        if let Some(cached) = self
            .cache
            .lock()
            .map_err(|_| "Credential cache is unavailable".to_string())?
            .get(account)
            .cloned()
        {
            return Ok(cached);
        }
        let value = self.get_platform(account)?;
        self.cache
            .lock()
            .map_err(|_| "Credential cache is unavailable".to_string())?
            .insert(account.to_string(), value.clone());
        Ok(value)
    }

    pub fn set(&self, account: &str, secret: &str) -> Result<(), String> {
        validate_account(account)?;
        let secret = secret.trim();
        if secret.is_empty() {
            return self.delete(account);
        }
        if secret.len() > 16_384 {
            return Err("Credential is unexpectedly large".to_string());
        }
        self.set_platform(account, secret)?;
        self.cache
            .lock()
            .map_err(|_| "Credential cache is unavailable".to_string())?
            .insert(account.to_string(), Some(secret.to_string()));
        Ok(())
    }

    pub fn delete(&self, account: &str) -> Result<(), String> {
        validate_account(account)?;
        self.delete_platform(account)?;
        self.cache
            .lock()
            .map_err(|_| "Credential cache is unavailable".to_string())?
            .insert(account.to_string(), None);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn get_platform(&self, account: &str) -> Result<Option<String>, String> {
        mac_keychain::get(account)
    }

    #[cfg(target_os = "macos")]
    fn set_platform(&self, account: &str, secret: &str) -> Result<(), String> {
        mac_keychain::set(account, secret)
    }

    #[cfg(target_os = "macos")]
    fn delete_platform(&self, account: &str) -> Result<(), String> {
        mac_keychain::delete(account)
    }

    #[cfg(target_os = "linux")]
    fn get_platform(&self, account: &str) -> Result<Option<String>, String> {
        let output = Command::new("secret-tool")
            .args(["lookup", "service", SERVICE, "account", account])
            .output()
            .map_err(|_| "Secret Service is unavailable. Install libsecret/secret-tool and unlock your keyring.".to_string())?;
        if output.status.success() {
            let value = String::from_utf8(output.stdout)
                .map_err(|_| "Secret Service returned invalid text".to_string())?;
            let value = value.trim_end_matches(['\r', '\n']).to_string();
            Ok((!value.is_empty()).then_some(value))
        } else {
            Ok(None)
        }
    }

    #[cfg(target_os = "linux")]
    fn set_platform(&self, account: &str, secret: &str) -> Result<(), String> {
        let mut child = Command::new("secret-tool")
            .args(["store", "--label", "OpenFlow API credential", "service", SERVICE, "account", account])
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
            .map_err(|_| "Secret Service is unavailable. Install libsecret/secret-tool and unlock your keyring.".to_string())?;
        child
            .stdin
            .take()
            .ok_or("Could not write to Secret Service")?
            .write_all(secret.as_bytes())
            .map_err(|e| format!("Could not write credential: {}", e))?;
        if child
            .wait()
            .map_err(|e| format!("Secret Service failed: {}", e))?
            .success()
        {
            Ok(())
        } else {
            Err("Secret Service refused the credential".to_string())
        }
    }

    #[cfg(target_os = "linux")]
    fn delete_platform(&self, account: &str) -> Result<(), String> {
        let status = Command::new("secret-tool")
            .args(["clear", "service", SERVICE, "account", account])
            .status()
            .map_err(|_| "Secret Service is unavailable".to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err("Secret Service could not delete the credential".to_string())
        }
    }

    #[cfg(target_os = "windows")]
    fn get_platform(&self, account: &str) -> Result<Option<String>, String> {
        let path = self.secret_path(account)?;
        if !path.exists() {
            return Ok(None);
        }
        let script = "$path=$env:OPENFLOW_SECRET_PATH;if([string]::IsNullOrEmpty($path)){throw 'Missing credential path'};$b=[IO.File]::ReadAllBytes($path);$p=[Security.Cryptography.ProtectedData]::Unprotect($b,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser);[Convert]::ToBase64String($p)";
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("OPENFLOW_SECRET_PATH", &path)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("Windows credential protection failed: {}", e))?;
        if !output.status.success() {
            return Err("Windows could not decrypt the credential".to_string());
        }
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(String::from_utf8_lossy(&output.stdout).trim())
            .map_err(|_| "Windows returned invalid credential data".to_string())?;
        String::from_utf8(decoded)
            .map(Some)
            .map_err(|_| "Credential is not valid text".to_string())
    }

    #[cfg(target_os = "windows")]
    fn set_platform(&self, account: &str, secret: &str) -> Result<(), String> {
        let path = self.secret_path(account)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create credential directory: {}", e))?;
        }
        let script = "[Console]::InputEncoding=[Text.Encoding]::UTF8;$path=$env:OPENFLOW_SECRET_PATH;if([string]::IsNullOrEmpty($path)){throw 'Missing credential path'};$s=[Console]::In.ReadToEnd();$b=[Text.Encoding]::UTF8.GetBytes($s);$p=[Security.Cryptography.ProtectedData]::Protect($b,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser);[IO.File]::WriteAllBytes($path,$p)";
        let mut child = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("OPENFLOW_SECRET_PATH", &path)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|e| format!("Windows credential protection failed: {}", e))?;
        child
            .stdin
            .take()
            .ok_or("Could not protect credential")?
            .write_all(secret.as_bytes())
            .map_err(|e| format!("Could not protect credential: {}", e))?;
        if child
            .wait()
            .map_err(|e| format!("Windows credential protection failed: {}", e))?
            .success()
        {
            Ok(())
        } else {
            Err("Windows could not protect the credential".to_string())
        }
    }

    #[cfg(target_os = "windows")]
    fn delete_platform(&self, account: &str) -> Result<(), String> {
        let path = self.secret_path(account)?;
        if !path.exists() {
            return Ok(());
        }
        std::fs::remove_file(path).map_err(|e| format!("Could not delete credential: {}", e))
    }

    #[cfg(target_os = "windows")]
    fn secret_path(&self, account: &str) -> Result<PathBuf, String> {
        validate_account(account)?;
        Ok(self
            .app_dir
            .join("secrets")
            .join(format!("{}.dpapi", account)))
    }
}

#[cfg(target_os = "macos")]
mod mac_keychain {
    use super::SERVICE;
    use std::ffi::{c_char, c_void};
    use std::ptr::{null, null_mut};

    type OSStatus = i32;
    type SecKeychainItemRef = *mut c_void;
    const ERR_ITEM_NOT_FOUND: OSStatus = -25_300;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecKeychainFindGenericPassword(
            keychain_or_array: *const c_void,
            service_name_length: u32,
            service_name: *const c_char,
            account_name_length: u32,
            account_name: *const c_char,
            password_length: *mut u32,
            password_data: *mut *mut c_void,
            item_ref: *mut SecKeychainItemRef,
        ) -> OSStatus;
        fn SecKeychainAddGenericPassword(
            keychain: *const c_void,
            service_name_length: u32,
            service_name: *const c_char,
            account_name_length: u32,
            account_name: *const c_char,
            password_length: u32,
            password_data: *const c_void,
            item_ref: *mut SecKeychainItemRef,
        ) -> OSStatus;
        fn SecKeychainItemModifyAttributesAndData(
            item_ref: SecKeychainItemRef,
            attributes: *const c_void,
            length: u32,
            data: *const c_void,
        ) -> OSStatus;
        fn SecKeychainItemDelete(item_ref: SecKeychainItemRef) -> OSStatus;
        fn SecKeychainItemFreeContent(attributes: *const c_void, data: *mut c_void) -> OSStatus;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(value: *const c_void);
    }

    pub fn get(account: &str) -> Result<Option<String>, String> {
        let mut length = 0_u32;
        let mut data = null_mut();
        let mut item = null_mut();
        // SAFETY: all byte slices remain alive for the call, lengths match, and
        // Security.framework owns the returned allocation until FreeContent.
        let status = unsafe {
            SecKeychainFindGenericPassword(
                null(),
                SERVICE.len() as u32,
                SERVICE.as_ptr().cast(),
                account.len() as u32,
                account.as_ptr().cast(),
                &mut length,
                &mut data,
                &mut item,
            )
        };
        if status == ERR_ITEM_NOT_FOUND {
            return Ok(None);
        }
        if status != 0 {
            return Err(format!("macOS Keychain read failed ({})", status));
        }
        // SAFETY: a successful lookup returns `length` readable bytes at `data`.
        let bytes =
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length as usize) }.to_vec();
        // SAFETY: these are the exact owned references returned by Security.framework.
        unsafe {
            let _ = SecKeychainItemFreeContent(null(), data);
            if !item.is_null() {
                CFRelease(item);
            }
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| "Keychain credential is not valid UTF-8".to_string())
    }

    pub fn set(account: &str, secret: &str) -> Result<(), String> {
        let mut item = null_mut();
        // SAFETY: input slices remain alive and lengths match. No password data
        // is placed in argv, environment variables, logs, or temporary files.
        let find_status = unsafe {
            SecKeychainFindGenericPassword(
                null(),
                SERVICE.len() as u32,
                SERVICE.as_ptr().cast(),
                account.len() as u32,
                account.as_ptr().cast(),
                null_mut(),
                null_mut(),
                &mut item,
            )
        };
        let status = if find_status == 0 {
            // SAFETY: `item` is a valid retained keychain item from the successful lookup.
            unsafe {
                SecKeychainItemModifyAttributesAndData(
                    item,
                    null(),
                    secret.len() as u32,
                    secret.as_ptr().cast(),
                )
            }
        } else if find_status == ERR_ITEM_NOT_FOUND {
            // SAFETY: input slices remain alive and their lengths match.
            unsafe {
                SecKeychainAddGenericPassword(
                    null(),
                    SERVICE.len() as u32,
                    SERVICE.as_ptr().cast(),
                    account.len() as u32,
                    account.as_ptr().cast(),
                    secret.len() as u32,
                    secret.as_ptr().cast(),
                    null_mut(),
                )
            }
        } else {
            find_status
        };
        // SAFETY: a successful find returned a retained Core Foundation reference.
        if !item.is_null() {
            unsafe {
                CFRelease(item);
            }
        }
        if status == 0 {
            Ok(())
        } else {
            Err(format!("macOS Keychain write failed ({})", status))
        }
    }

    pub fn delete(account: &str) -> Result<(), String> {
        let mut item = null_mut();
        // SAFETY: input slices remain alive and lengths match.
        let status = unsafe {
            SecKeychainFindGenericPassword(
                null(),
                SERVICE.len() as u32,
                SERVICE.as_ptr().cast(),
                account.len() as u32,
                account.as_ptr().cast(),
                null_mut(),
                null_mut(),
                &mut item,
            )
        };
        if status == ERR_ITEM_NOT_FOUND {
            return Ok(());
        }
        if status != 0 {
            return Err(format!("macOS Keychain lookup failed ({})", status));
        }
        // SAFETY: item is valid after a successful find.
        let delete_status = unsafe { SecKeychainItemDelete(item) };
        // SAFETY: successful find returned a retained Core Foundation reference.
        unsafe {
            CFRelease(item);
        }
        if delete_status == 0 {
            Ok(())
        } else {
            Err(format!("macOS Keychain delete failed ({})", delete_status))
        }
    }
}

fn validate_account(account: &str) -> Result<(), String> {
    let path = std::path::Path::new(account);
    if account.is_empty()
        || account.len() > 64
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || !account
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err("Invalid credential name".to_string());
    }
    Ok(())
}
