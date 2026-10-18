//! Bridge AppKit quit to the same asynchronous save/close path as window close.
//!
//! Tao 0.37.1 does not implement applicationShouldTerminate:, and its
//! applicationWillTerminate: notification is too late to cancel quitting.
//! Keep its delegate and ivar layout intact; fail startup if that contract
//! changes instead of replacing a framework implementation.
use std::{
    ffi::CString,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::OnceLock,
};

use objc2::{
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel, Encode, MainThreadMarker,
};
use objc2_app_kit::{NSApplication, NSApplicationTerminateReply};
use tauri::{AppHandle, Emitter, Manager};

static APP: OnceLock<AppHandle> = OnceLock::new();

extern "C-unwind" fn should_terminate(
    _delegate: &AnyObject,
    _selector: Sel,
    _sender: &NSApplication,
) -> NSApplicationTerminateReply {
    // Nothing may unwind into AppKit. A missing context or failed notification
    // must not permit termination while the application might contain edits.
    catch_unwind(AssertUnwindSafe(|| {
        let Some(app) = APP.get() else {
            return NSApplicationTerminateReply::TerminateCancel;
        };
        let Some(gate) = app.try_state::<crate::CloseGate>() else {
            return NSApplicationTerminateReply::TerminateCancel;
        };
        if gate.needs_confirmation() {
            let _ = app.emit("cuenote:close-requested", ());
            NSApplicationTerminateReply::TerminateCancel
        } else {
            NSApplicationTerminateReply::TerminateNow
        }
    }))
    .unwrap_or(NSApplicationTerminateReply::TerminateCancel)
}

fn add_termination_method(class: &AnyClass) -> Result<(), &'static str> {
    let selector = sel!(applicationShouldTerminate:);
    // Includes inherited methods: class_addMethod alone would shadow one.
    if class.instance_method(selector).is_some() {
        return Err("macOS quit delegate already has a termination handler");
    }
    let encoding = CString::new(format!("{}@:@", NSApplicationTerminateReply::ENCODING))
        .map_err(|_| "Invalid macOS termination method encoding")?;
    // SAFETY: AppKit sends (self, _cmd, NSApplication*) and expects an
    // NSUInteger-backed NSApplicationTerminateReply, not an Objective-C BOOL.
    // The function and its OnceLock context live until process termination.
    let implementation: Imp = unsafe {
        std::mem::transmute::<
            extern "C-unwind" fn(&AnyObject, Sel, &NSApplication) -> NSApplicationTerminateReply,
            Imp,
        >(should_terminate)
    };
    // SAFETY: The runtime permits adding methods to registered classes. This
    // does not change the delegate's class, instance layout, or existing methods.
    let added = unsafe {
        objc2::ffi::class_addMethod(
            class as *const AnyClass as *mut AnyClass,
            selector,
            implementation,
            encoding.as_ptr(),
        )
    };
    if !added.as_bool() {
        return Err("Cannot install macOS termination handler");
    }
    let method = class
        .instance_method(selector)
        .ok_or("macOS termination handler was not registered")?;
    if method.implementation() as usize != implementation as usize
        || method.arguments_count() != 3
        || method.return_type().to_bytes()
            != NSApplicationTerminateReply::ENCODING.to_string().as_bytes()
    {
        return Err("macOS termination handler registration differs from its ABI");
    }
    Ok(())
}

pub(crate) fn install(app: &AppHandle) -> Result<(), &'static str> {
    let mtm = MainThreadMarker::new().ok_or("macOS quit setup requires the main thread")?;
    let application = NSApplication::sharedApplication(mtm);
    // Retain the exact delegate across setter calls, including the nil interval.
    let delegate = application
        .delegate()
        .ok_or("Missing macOS application delegate")?;
    let delegate_object: &AnyObject = (*delegate).as_ref();
    let class = delegate_object.class();
    if class.name().to_bytes() != b"TaoAppDelegateParent" {
        return Err("Unsupported macOS application delegate; quit protection was not installed");
    }
    APP.set(app.clone())
        .map_err(|_| "macOS quit protection was already initialized")?;
    add_termination_method(class)?;
    // Refresh AppKit's optional delegate-method flags using its public setter.
    // The original Tao delegate and its auxState remain the same object.
    application.setDelegate(None);
    application.setDelegate(Some(&delegate));
    let current = application
        .delegate()
        .ok_or("macOS application delegate disappeared")?;
    let current_object: &AnyObject = (*current).as_ref();
    if !std::ptr::eq(delegate_object, current_object) {
        return Err("macOS application delegate changed during quit setup");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::{class, runtime::ClassBuilder};

    // These exercise the Objective-C runtime boundary, including inherited
    // selectors. No application, global close context or user files are needed.
    #[test]
    fn refuses_to_replace_or_shadow_an_existing_termination_handler() {
        let base = ClassBuilder::new(c"CueNoteQuitAdapterTestBase", class!(NSObject))
            .unwrap()
            .register();
        add_termination_method(base).unwrap();
        let selector = sel!(applicationShouldTerminate:);
        let method = base.instance_method(selector).unwrap();
        assert_eq!(method.return_type().to_str().unwrap(), "Q");
        assert!(add_termination_method(base).is_err());
        let child = ClassBuilder::new(c"CueNoteQuitAdapterTestChild", base)
            .unwrap()
            .register();
        assert!(add_termination_method(child).is_err());
    }
}
