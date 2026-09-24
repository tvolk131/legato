//! Catching a file drag at the edge of the screen.
//!
//! Windows has no API for "what is being dragged right now". Instead, while the left
//! button is held against an edge that leads to another machine, the capture window is
//! placed under the cursor. It's an OLE drop target, so an ongoing file drag enters it and
//! hands over the file list. After that the drag carries on over our window (the drop
//! lands on it, harmlessly) while the cursor moves to the other machine.

use std::cell::RefCell;
use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, LPARAM, POINTL, WPARAM};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl,
    RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows_core::{Ref, implement};

/// Posted to the capture window when a drag carrying files has entered it.
pub(crate) const WM_FILES_ENTERED: u32 = WM_APP + 1;

thread_local! {
    /// Files from the latest drag that entered the catcher, until the capture thread
    /// picks them up.
    pub(crate) static ENTERED: RefCell<Option<Vec<PathBuf>>> = const { RefCell::new(None) };
}

#[implement(IDropTarget)]
struct Catcher {
    window: HWND,
}

impl IDropTarget_Impl for Catcher_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let files = data.as_ref().map(files_in).unwrap_or_default();
        let accept = !files.is_empty();
        if accept {
            ENTERED.with_borrow_mut(|e| *e = Some(files));
            // SAFETY: posting to our own window.
            let _ =
                unsafe { PostMessageW(Some(self.window), WM_FILES_ENTERED, WPARAM(0), LPARAM(0)) };
        }
        set(
            effect,
            if accept {
                DROPEFFECT_COPY
            } else {
                DROPEFFECT_NONE
            },
        );
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        set(effect, DROPEFFECT_COPY);
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Drop(
        &self,
        _data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        // The real drop happens on the other machine; nothing to do here.
        set(effect, DROPEFFECT_NONE);
        Ok(())
    }
}

fn set(effect: *mut DROPEFFECT, value: DROPEFFECT) {
    if !effect.is_null() {
        // SAFETY: OLE passes a valid out-pointer.
        unsafe { *effect = value };
    }
}

/// The paths in a data object's `CF_HDROP`, if it has one.
fn files_in(data: &IDataObject) -> Vec<PathBuf> {
    let format = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    // SAFETY: standard IDataObject/HDROP use; the medium is released afterwards.
    unsafe {
        let Ok(mut medium) = data.GetData(&format) else {
            return Vec::new();
        };
        let hdrop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let mut files = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let copied = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            files.push(PathBuf::from(String::from_utf16_lossy(&buf[..copied])));
        }
        ReleaseStgMedium(&mut medium);
        files
    }
}

/// Makes `window` a drop target. OLE must be initialised on this thread.
pub(crate) fn register(window: HWND) -> windows::core::Result<()> {
    let target: IDropTarget = Catcher { window }.into();
    // SAFETY: the window belongs to this thread; OLE keeps its own reference.
    unsafe { RegisterDragDrop(window, &target) }
}

pub(crate) fn unregister(window: HWND) {
    // SAFETY: undoes `register`.
    let _ = unsafe { RevokeDragDrop(window) };
}
