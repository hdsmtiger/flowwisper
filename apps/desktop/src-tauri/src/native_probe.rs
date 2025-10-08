use std::time::Duration;

#[derive(Debug, Clone)]
pub struct NativeProbeObservation {
    pub supported: bool,
    pub latency: Option<Duration>,
    pub raw_latency_ns: Option<u128>,
    pub user_reaction: Option<Duration>,
    pub within_sla: Option<bool>,
    pub device_origin: Option<String>,
    pub interface: &'static str,
    pub reason: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("Fn 探测超时")]
    Timeout,
    #[error("{0}")]
    Io(String),
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{NativeProbeObservation, ProbeError};
    use core_foundation::array::CFArray;
    use core_foundation::base::{kCFAllocatorDefault, CFRelease, CFTypeID, CFTypeRef, TCFType};
    use core_foundation::dictionary::{CFDictionary, CFMutableDictionary};
    use core_foundation::number::CFNumber;
    use core_foundation::runloop::{
        kCFRunLoopDefaultMode, CFRunLoopGetCurrent, CFRunLoopRef, CFRunLoopRunInMode, CFRunLoopStop,
    };
    use core_foundation::string::{CFString, CFStringRef};
    use io_kit_sys::hid::base::{IOHIDDeviceRef, IOHIDValueRef};
    use io_kit_sys::hid::device::IOHIDDeviceGetProperty;
    use io_kit_sys::hid::element::{
        IOHIDElementGetDevice, IOHIDElementGetUsage, IOHIDElementGetUsagePage,
    };
    use io_kit_sys::hid::keys::kIOHIDOptionsTypeNone;
    use io_kit_sys::hid::manager::{
        IOHIDManagerClose, IOHIDManagerCreate, IOHIDManagerOpen,
        IOHIDManagerRegisterInputValueCallback, IOHIDManagerScheduleWithRunLoop,
        IOHIDManagerSetDeviceMatchingMultiple, IOHIDManagerUnscheduleFromRunLoop,
    };
    use io_kit_sys::hid::usage_tables::kHIDPage_KeyboardOrKeypad;
    use io_kit_sys::hid::value::{IOHIDValueGetElement, IOHIDValueGetTimeStamp};
    use io_kit_sys::ret::kIOReturnSuccess;
    use mach::mach_time::{mach_absolute_time, mach_timebase_info, mach_timebase_info_data_t};
    use std::ffi::c_void;
    use std::sync::mpsc;
    use std::time::Duration;

    const APPLE_VENDOR_PAGE: u32 = 0xFF00;
    const APPLE_FN_USAGE: u32 = 0x0003;

    enum Event {
        Fn {
            latency: Duration,
            raw_ns: u128,
            reaction: Duration,
            device_origin: Option<String>,
        },
        Other(String),
        Error(String),
    }

    struct Context {
        sender: mpsc::Sender<Event>,
        run_loop: CFRunLoopRef,
        timebase: mach_timebase_info_data_t,
        prompt_ts: u64,
    }

    impl Context {
        fn convert(&self, delta: u64) -> (Duration, u128) {
            let nanos = delta as u128 * self.timebase.numer as u128 / self.timebase.denom as u128;
            let nanos_clamped = nanos.min(u64::MAX as u128);
            (Duration::from_nanos(nanos_clamped as u64), nanos)
        }
    }

    unsafe extern "C" fn input_callback(
        context: *mut c_void,
        result: i32,
        _: *mut c_void,
        value: IOHIDValueRef,
    ) {
        if context.is_null() {
            return;
        }
        let ctx = &*(context as *const Context);
        if result != kIOReturnSuccess as i32 {
            let _ = ctx
                .sender
                .send(Event::Error(format!("IOHID 回调错误: 0x{result:X}")));
            CFRunLoopStop(ctx.run_loop);
            return;
        }

        let element = IOHIDValueGetElement(value);
        if element.is_null() {
            let _ = ctx.sender.send(Event::Error("缺少 HID 元素".into()));
            CFRunLoopStop(ctx.run_loop);
            return;
        }

        let usage_page = IOHIDElementGetUsagePage(element) as u32;
        let usage = IOHIDElementGetUsage(element) as u32;

        if usage_page == APPLE_VENDOR_PAGE && usage == APPLE_FN_USAGE {
            let now = mach_absolute_time();
            let event_ts = IOHIDValueGetTimeStamp(value);
            let delta = now.saturating_sub(event_ts);
            let (latency, nanos) = ctx.convert(delta);
            let reaction_delta = event_ts.saturating_sub(ctx.prompt_ts);
            let (reaction, _) = ctx.convert(reaction_delta);
            let device = IOHIDElementGetDevice(element);
            let origin = if device.is_null() {
                None
            } else {
                extract_origin(device)
            };
            let _ = ctx.sender.send(Event::Fn {
                latency,
                raw_ns: nanos,
                reaction,
                device_origin: origin,
            });
            CFRunLoopStop(ctx.run_loop);
            return;
        }

        if usage_page == kHIDPage_KeyboardOrKeypad as u32 {
            if let Some(label) = describe_key(usage) {
                let _ = ctx.sender.send(Event::Other(label));
            }
            CFRunLoopStop(ctx.run_loop);
        }
    }

    unsafe fn extract_origin(device: IOHIDDeviceRef) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        let transport_key = CFString::from_static_string("Transport");
        if let Some(value) = copy_string_property(device, transport_key.as_concrete_TypeRef()) {
            parts.push(value);
        }
        let product_key = CFString::from_static_string("Product");
        if let Some(value) = copy_string_property(device, product_key.as_concrete_TypeRef()) {
            parts.push(value);
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("/"))
        }
    }

    unsafe fn copy_string_property(device: IOHIDDeviceRef, key: CFStringRef) -> Option<String> {
        let value = IOHIDDeviceGetProperty(device, key);
        if value.is_null() {
            return None;
        }
        if CFGetTypeID(value) == CFString::type_id() {
            Some(CFString::wrap_under_get_rule(value as CFStringRef).to_string())
        } else {
            None
        }
    }

    unsafe fn CFGetTypeID(value: CFTypeRef) -> CFTypeID {
        core_foundation_sys::base::CFGetTypeID(value)
    }

    fn describe_key(usage: u32) -> Option<String> {
        match usage {
            0x04..=0x1d => {
                let ch = ((usage - 0x04) as u8 + b'a') as char;
                Some(ch.to_uppercase().collect())
            }
            0x1e..=0x27 => Some((usage - 0x1d).to_string()),
            0x28 => Some("Enter".into()),
            0x29 => Some("Esc".into()),
            0x2a => Some("Backspace".into()),
            0x2b => Some("Tab".into()),
            0x2c => Some("Space".into()),
            0x39 => Some("CapsLock".into()),
            _ => Some(format!("Usage(0x{usage:X})")),
        }
    }

    fn matching_dictionary(usage_page: u32, usage: u32) -> CFDictionary<CFString, CFNumber> {
        let mut dict: CFMutableDictionary<CFString, CFNumber> = CFMutableDictionary::new();
        let page_key = CFString::from_static_string("DeviceUsagePage");
        let usage_key = CFString::from_static_string("DeviceUsage");
        dict.set(page_key, CFNumber::from(usage_page as i32));
        dict.set(usage_key, CFNumber::from(usage as i32));
        dict.to_immutable()
    }

    fn probe_via_iohid(timeout: Duration) -> Result<NativeProbeObservation, ProbeError> {
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || unsafe {
            let manager = IOHIDManagerCreate(kCFAllocatorDefault, kIOHIDOptionsTypeNone);
            if manager.is_null() {
                let _ = tx.send(Event::Error("创建 IOHIDManager 失败".into()));
                return;
            }

            let matches = vec![matching_dictionary(APPLE_VENDOR_PAGE, APPLE_FN_USAGE)];
            let array = CFArray::from_CFTypes(&matches);
            IOHIDManagerSetDeviceMatchingMultiple(manager, array.as_concrete_TypeRef());

            let mut timebase = mach_timebase_info_data_t { numer: 0, denom: 0 };
            mach_timebase_info(&mut timebase);

            let run_loop = CFRunLoopGetCurrent();
            let ctx = Box::new(Context {
                sender: tx.clone(),
                run_loop,
                timebase,
                prompt_ts: mach_absolute_time(),
            });
            let ctx_ptr = Box::into_raw(ctx);

            IOHIDManagerScheduleWithRunLoop(manager, run_loop, kCFRunLoopDefaultMode);
            let open_status = IOHIDManagerOpen(manager, kIOHIDOptionsTypeNone);
            if open_status != kIOReturnSuccess {
                let _ = (*ctx_ptr).sender.send(Event::Error(format!(
                    "打开 IOHIDManager 失败: 0x{open_status:X}"
                )));
                IOHIDManagerUnscheduleFromRunLoop(manager, run_loop, kCFRunLoopDefaultMode);
                CFRelease(manager as CFTypeRef);
                drop(Box::from_raw(ctx_ptr));
                return;
            }

            IOHIDManagerRegisterInputValueCallback(manager, input_callback, ctx_ptr as *mut c_void);
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, timeout.as_secs_f64(), 0u8);
            IOHIDManagerUnscheduleFromRunLoop(manager, run_loop, kCFRunLoopDefaultMode);
            IOHIDManagerClose(manager, kIOHIDOptionsTypeNone);
            CFRelease(manager as CFTypeRef);
            drop(Box::from_raw(ctx_ptr));
        });

        let outcome = match rx.recv_timeout(timeout + Duration::from_millis(50)) {
            Ok(Event::Fn {
                latency,
                raw_ns,
                reaction,
                device_origin,
            }) => {
                let sla = Duration::from_millis(400);
                let within_sla = reaction <= sla;
                NativeProbeObservation {
                    supported: true,
                    latency: Some(latency),
                    raw_latency_ns: Some(raw_ns),
                    user_reaction: Some(reaction),
                    within_sla: Some(within_sla),
                    device_origin,
                    interface: "IOHID",
                    reason: if within_sla {
                        None
                    } else {
                        Some(format!(
                            "Fn 驱动回调耗时 {}ms，超出 {}ms SLA",
                            reaction.as_millis(),
                            sla.as_millis()
                        ))
                    },
                }
            }
            Ok(Event::Other(label)) => NativeProbeObservation {
                supported: false,
                latency: None,
                raw_latency_ns: None,
                user_reaction: None,
                within_sla: None,
                device_origin: None,
                interface: "IOHID",
                reason: Some(format!("检测到按键 {label}，未捕获到 Fn")),
            },
            Ok(Event::Error(reason)) => {
                return Err(ProbeError::Io(reason));
            }
            Err(_) => {
                return Err(ProbeError::Timeout);
            }
        };

        let _ = handle.join();
        Ok(outcome)
    }

    fn probe_via_karabiner(timeout: Duration) -> Result<NativeProbeObservation, ProbeError> {
        use karabiner_driverkit::{self as karabiner, DKEvent};
        use std::sync::mpsc;
        use std::time::Instant;

        if !karabiner::driver_activated() {
            return Ok(NativeProbeObservation {
                supported: false,
                latency: None,
                raw_latency_ns: None,
                user_reaction: None,
                within_sla: None,
                device_origin: None,
                interface: "Karabiner",
                reason: Some("Karabiner 虚拟设备未激活".into()),
            });
        }

        if !karabiner::register_device("") {
            return Ok(NativeProbeObservation {
                supported: false,
                latency: None,
                raw_latency_ns: None,
                user_reaction: None,
                within_sla: None,
                device_origin: None,
                interface: "Karabiner",
                reason: Some("Karabiner 注册虚拟键盘失败".into()),
            });
        }

        if !karabiner::grab() {
            return Ok(NativeProbeObservation {
                supported: false,
                latency: None,
                raw_latency_ns: None,
                user_reaction: None,
                within_sla: None,
                device_origin: None,
                interface: "Karabiner",
                reason: Some("Karabiner 无法获取虚拟键盘控制权".into()),
            });
        }

        let (event_tx, event_rx) = mpsc::channel();
        let start = Instant::now();

        let wait_handle = std::thread::spawn(move || {
            let mut event = DKEvent {
                value: 0,
                page: 0,
                code: 0,
            };
            let status = karabiner::wait_key(&mut event);
            karabiner::release();
            if status != 0 {
                let _ = event_tx.send(Err(ProbeError::Io(format!(
                    "Karabiner 事件等待失败: {status}"
                ))));
                return;
            }

            if event.page == APPLE_VENDOR_PAGE && event.code == APPLE_FN_USAGE {
                let reaction = start.elapsed();
                let sla = Duration::from_millis(400);
                let within_sla = reaction <= sla;
                let _ = event_tx.send(Ok(NativeProbeObservation {
                    supported: true,
                    latency: None,
                    raw_latency_ns: None,
                    user_reaction: Some(reaction),
                    within_sla: Some(within_sla),
                    device_origin: Some("KarabinerVirtual".into()),
                    interface: "Karabiner",
                    reason: if within_sla {
                        Some("IOHID 捕获失败，使用 Karabiner 虚拟设备完成探测".into())
                    } else {
                        Some(format!(
                            "Karabiner 退化路径回调 {}ms，超出 {}ms SLA",
                            reaction.as_millis(),
                            sla.as_millis()
                        ))
                    },
                }));
            } else {
                let _ = event_tx.send(Ok(NativeProbeObservation {
                    supported: false,
                    latency: None,
                    raw_latency_ns: None,
                    user_reaction: None,
                    within_sla: None,
                    device_origin: Some("KarabinerVirtual".into()),
                    interface: "Karabiner",
                    reason: Some(format!(
                        "Karabiner 捕获到非 Fn 按键 (page: 0x{:X}, code: 0x{:X})",
                        event.page, event.code
                    )),
                }));
            }
        });

        let result = match event_rx.recv_timeout(timeout) {
            Ok(Ok(observation)) => Ok(observation),
            Ok(Err(err)) => Err(err),
            Err(_) => {
                karabiner::release();
                Err(ProbeError::Timeout)
            }
        };

        let _ = wait_handle.join();
        result
    }

    pub fn probe_fn(timeout: Duration) -> Result<NativeProbeObservation, ProbeError> {
        let primary_budget = if timeout > Duration::from_millis(240) {
            Duration::from_millis(240)
        } else if timeout > Duration::from_millis(120) {
            timeout - Duration::from_millis(120)
        } else {
            timeout / 2
        };
        let fallback_budget = timeout
            .checked_sub(primary_budget)
            .unwrap_or(Duration::ZERO);

        match probe_via_iohid(primary_budget) {
            Ok(observation) if observation.supported => Ok(observation),
            Ok(mut observation) => {
                if fallback_budget.is_zero() {
                    if observation.reason.is_none() {
                        observation.reason = Some(format!(
                            "IOHID 未在 {}ms 内捕获 Fn",
                            primary_budget.as_millis()
                        ));
                    }
                    return Ok(observation);
                }

                let io_reason = observation.reason.clone();
                match probe_via_karabiner(fallback_budget) {
                    Ok(mut fallback) => {
                        if fallback.supported {
                            fallback.reason = Some(match (fallback.reason.take(), io_reason) {
                                (Some(fallback_reason), Some(io_reason)) => {
                                    format!("{io_reason}；{fallback_reason}")
                                }
                                (Some(fallback_reason), None) => fallback_reason,
                                (None, Some(io_reason)) => {
                                    format!("{io_reason}；已回退至 Karabiner 完成探测")
                                }
                                (None, None) => "已回退至 Karabiner 完成探测".into(),
                            });
                        } else {
                            fallback.reason = Some(match (fallback.reason.take(), io_reason) {
                                (Some(fallback_reason), Some(io_reason)) => {
                                    format!(
                                        "{io_reason}；Karabiner 退化仍失败（{fallback_reason}）"
                                    )
                                }
                                (Some(fallback_reason), None) => {
                                    format!("Karabiner 退化仍失败（{fallback_reason}）")
                                }
                                (None, Some(io_reason)) => {
                                    format!("{io_reason}；Karabiner 退化仍失败")
                                }
                                (None, None) => "Karabiner 退化仍失败".into(),
                            });
                        }
                        Ok(fallback)
                    }
                    Err(fallback_err) => {
                        let reason = match (&io_reason, &fallback_err) {
                            (Some(io_reason), ProbeError::Timeout) => {
                                format!(
                                    "{io_reason}；Karabiner 退化在 {}ms 内超时",
                                    fallback_budget.as_millis()
                                )
                            }
                            (Some(io_reason), ProbeError::Io(msg)) => {
                                format!("{io_reason}；Karabiner 退化失败：{msg}")
                            }
                            (None, ProbeError::Timeout) => {
                                format!(
                                    "IOHID 未捕获 Fn；Karabiner 退化在 {}ms 内超时",
                                    fallback_budget.as_millis()
                                )
                            }
                            (None, ProbeError::Io(msg)) => {
                                format!("IOHID 未捕获 Fn；Karabiner 退化失败：{msg}")
                            }
                        };
                        Ok(NativeProbeObservation {
                            supported: false,
                            latency: None,
                            raw_latency_ns: None,
                            user_reaction: None,
                            within_sla: None,
                            device_origin: None,
                            interface: "IOHID/Karabiner",
                            reason: Some(reason),
                        })
                    }
                }
            }
            Err(io_err) => {
                if fallback_budget.is_zero() {
                    let reason = match io_err {
                        ProbeError::Timeout => {
                            format!("IOHID 捕获在 {}ms 预算内超时", primary_budget.as_millis())
                        }
                        ProbeError::Io(msg) => format!("IOHID 捕获失败：{msg}"),
                    };
                    return Ok(NativeProbeObservation {
                        supported: false,
                        latency: None,
                        raw_latency_ns: None,
                        user_reaction: None,
                        within_sla: None,
                        device_origin: None,
                        interface: "IOHID",
                        reason: Some(reason),
                    });
                }

                match probe_via_karabiner(fallback_budget) {
                    Ok(mut observation) => {
                        observation.reason = Some(match (observation.reason.take(), &io_err) {
                            (Some(fallback_reason), ProbeError::Timeout) => {
                                format!("IOHID 捕获超时；{fallback_reason}")
                            }
                            (Some(fallback_reason), ProbeError::Io(msg)) => {
                                format!("IOHID 错误：{msg}；{fallback_reason}")
                            }
                            (None, ProbeError::Timeout) => {
                                format!(
                                    "IOHID 捕获在 {}ms 内超时；已回退至 Karabiner",
                                    primary_budget.as_millis()
                                )
                            }
                            (None, ProbeError::Io(msg)) => {
                                format!("IOHID 错误：{msg}；已回退至 Karabiner")
                            }
                        });
                        Ok(observation)
                    }
                    Err(fallback_err) => {
                        let reason = match (&io_err, &fallback_err) {
                            (ProbeError::Timeout, ProbeError::Timeout) => format!(
                                "IOHID 捕获在 {}ms 内超时；Karabiner 退化在 {}ms 内同样超时",
                                primary_budget.as_millis(),
                                fallback_budget.as_millis()
                            ),
                            (ProbeError::Timeout, ProbeError::Io(msg)) => {
                                format!("IOHID 捕获超时；Karabiner 退化失败：{msg}")
                            }
                            (ProbeError::Io(msg), ProbeError::Timeout) => format!(
                                "IOHID 错误：{msg}；Karabiner 退化在 {}ms 内超时",
                                fallback_budget.as_millis()
                            ),
                            (ProbeError::Io(msg), ProbeError::Io(fallback_msg)) => {
                                format!("IOHID 错误：{msg}；Karabiner 退化失败：{fallback_msg}")
                            }
                        };
                        Ok(NativeProbeObservation {
                            supported: false,
                            latency: None,
                            raw_latency_ns: None,
                            user_reaction: None,
                            within_sla: None,
                            device_origin: None,
                            interface: "IOHID/Karabiner",
                            reason: Some(reason),
                        })
                    }
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{NativeProbeObservation, ProbeError};
    use once_cell::sync::OnceCell;
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::sync::{mpsc, Mutex};
    use std::time::{Duration, Instant};
    use windows::core::w;
    use windows::Win32::Foundation::{
        GetLastError, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HRAWINPUT, HWND, LPARAM, LRESULT,
        WPARAM,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::SystemInformation::GetSystemMetrics;
    use windows::Win32::System::SystemServices::SM_REMOTESESSION;
    use windows::Win32::System::Time::{
        GetTickCount64, QueryPerformanceCounter, QueryPerformanceFrequency,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetRawInputData, GetRawInputDeviceInfoW, RegisterRawInputDevices, KBDLLHOOKSTRUCT,
        LLKHF_EXTENDED, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RAWKEYBOARD, RIDEV_INPUTSINK,
        RIDI_DEVICENAME, RID_INPUT, RIM_TYPEKEYBOARD, WH_KEYBOARD_LL,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GetMessageTime, PeekMessageW, PostQuitMessage, RegisterClassW, SetWindowsHookExW,
        TranslateMessage, UnhookWindowsHookEx, HC_ACTION, HWND_MESSAGE, MSG, PM_REMOVE, WM_INPUT,
        WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN, WNDCLASSW,
    };

    const SLA_MS: u64 = 400;

    enum HookEvent {
        Fn {
            latency: Duration,
            raw_ns: u128,
            reaction: Duration,
            device_origin: Option<String>,
        },
        Other(String),
        Error(String),
    }

    struct HookContext {
        sender: mpsc::Sender<HookEvent>,
        start_tick: u64,
        start_counter: i64,
        frequency: i64,
    }

    static HOOK_CONTEXT: OnceCell<Mutex<Option<*mut HookContext>>> = OnceCell::new();

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION {
            if let Some(storage) = HOOK_CONTEXT.get() {
                if let Ok(guard) = storage.lock() {
                    if let Some(ptr) = *guard {
                        let ctx = &*ptr;
                        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
                        if wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN {
                            let (latency, raw_ns, reaction) = compute_timings(
                                ctx.start_tick,
                                ctx.start_counter,
                                ctx.frequency,
                                kb.time,
                            );
                            if is_fn_key(kb) {
                                let origin = if kb.flags & LLKHF_EXTENDED != 0 {
                                    Some("External/Extended".to_string())
                                } else if GetSystemMetrics(SM_REMOTESESSION) != 0 {
                                    Some("RemoteSession".to_string())
                                } else {
                                    Some("Internal".to_string())
                                };
                                let _ = ctx.sender.send(HookEvent::Fn {
                                    latency,
                                    raw_ns,
                                    reaction,
                                    device_origin: origin,
                                });
                                PostQuitMessage(0);
                            } else {
                                let label = describe_key(kb.vkCode);
                                let _ = ctx.sender.send(HookEvent::Other(label));
                                PostQuitMessage(0);
                            }
                        }
                    }
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    enum RawEvent {
        Fn {
            latency: Duration,
            raw_ns: u128,
            reaction: Duration,
            device_origin: Option<String>,
        },
        Other(String),
        Error(String),
    }

    struct RawContext {
        sender: mpsc::Sender<RawEvent>,
        start_tick: u64,
        start_counter: i64,
        frequency: i64,
    }

    static RAW_CONTEXT: OnceCell<Mutex<Option<*mut RawContext>>> = OnceCell::new();

    unsafe extern "system" fn raw_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_INPUT => {
                if let Some(storage) = RAW_CONTEXT.get() {
                    if let Ok(guard) = storage.lock() {
                        if let Some(ptr) = *guard {
                            let ctx = &*ptr;
                            let mut size = 0u32;
                            if GetRawInputData(
                                HRAWINPUT(lparam.0 as isize),
                                RID_INPUT,
                                None,
                                &mut size,
                                size_of::<RAWINPUTHEADER>() as u32,
                            ) == u32::MAX
                            {
                                let _ = ctx
                                    .sender
                                    .send(RawEvent::Error("读取 Raw Input 长度失败".into()));
                                PostQuitMessage(0);
                                return LRESULT(0);
                            }
                            let mut buffer = vec![0u8; size as usize];
                            if GetRawInputData(
                                HRAWINPUT(lparam.0 as isize),
                                RID_INPUT,
                                Some(buffer.as_mut_ptr() as *mut c_void),
                                &mut size,
                                size_of::<RAWINPUTHEADER>() as u32,
                            ) == u32::MAX
                            {
                                let _ = ctx
                                    .sender
                                    .send(RawEvent::Error("读取 Raw Input 数据失败".into()));
                                PostQuitMessage(0);
                                return LRESULT(0);
                            }
                            let raw = &*(buffer.as_ptr() as *const RAWINPUT);
                            if raw.header.dwType != RIM_TYPEKEYBOARD {
                                return DefWindowProcW(hwnd, msg, wparam, lparam);
                            }
                            let kb = unsafe { raw.data.keyboard };
                            if kb.Flags
                                & windows::Win32::UI::WindowsAndMessaging::RI_KEY_BREAK as u16
                                != 0
                            {
                                return DefWindowProcW(hwnd, msg, wparam, lparam);
                            }
                            let (latency, raw_ns, reaction) = compute_timings(
                                ctx.start_tick,
                                ctx.start_counter,
                                ctx.frequency,
                                GetMessageTime() as u32,
                            );
                            if is_raw_fn_key(&kb) {
                                let origin = raw_device_origin(raw.header.hDevice);
                                let _ = ctx.sender.send(RawEvent::Fn {
                                    latency,
                                    raw_ns,
                                    reaction,
                                    device_origin: origin,
                                });
                                PostQuitMessage(0);
                            } else {
                                let label = describe_key(kb.VKey as u32);
                                let _ = ctx.sender.send(RawEvent::Other(label));
                                PostQuitMessage(0);
                            }
                        }
                    }
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    pub fn probe_fn(timeout: Duration) -> Result<NativeProbeObservation, ProbeError> {
        let primary_budget = if timeout > Duration::from_millis(240) {
            Duration::from_millis(240)
        } else if timeout > Duration::from_millis(120) {
            timeout - Duration::from_millis(120)
        } else {
            timeout / 2
        };
        let fallback_budget = timeout
            .checked_sub(primary_budget)
            .unwrap_or(Duration::ZERO);

        match probe_with_low_level_hook(primary_budget) {
            Ok(result) if result.supported => Ok(result),
            Ok(mut result) => {
                if fallback_budget.is_zero() {
                    result.reason.get_or_insert_with(|| {
                        format!("低层钩子未在 {}ms 内捕获 Fn", primary_budget.as_millis())
                    });
                    return Ok(result);
                }
                match probe_with_raw_input(fallback_budget) {
                    Ok(fallback) => Ok(fallback),
                    Err(err) => {
                        result.reason = Some(match result.reason.take() {
                            Some(existing) => format!("{existing}；Raw Input 退化失败: {err}"),
                            None => format!("Raw Input 退化失败: {err}"),
                        });
                        Ok(result)
                    }
                }
            }
            Err(primary_err) => {
                if fallback_budget.is_zero() {
                    return Err(primary_err);
                }
                match probe_with_raw_input(fallback_budget) {
                    Ok(fallback) => Ok(fallback),
                    Err(_) => Err(primary_err),
                }
            }
        }
    }

    fn probe_with_low_level_hook(timeout: Duration) -> Result<NativeProbeObservation, ProbeError> {
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || unsafe {
            let mut freq = 0i64;
            QueryPerformanceFrequency(&mut freq);
            let mut start_counter = 0i64;
            QueryPerformanceCounter(&mut start_counter);
            let start_tick = GetTickCount64();

            let context = Box::new(HookContext {
                sender: tx.clone(),
                start_tick,
                start_counter,
                frequency: freq,
            });
            let ctx_ptr = Box::into_raw(context);
            let storage = HOOK_CONTEXT.get_or_init(|| Mutex::new(None));
            {
                let mut guard = storage.lock().unwrap();
                *guard = Some(ctx_ptr);
            }

            let module = GetModuleHandleW(None).unwrap_or(HINSTANCE::default());
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0);
            if hook.is_invalid() {
                let _ = (*ctx_ptr)
                    .sender
                    .send(HookEvent::Error("注册键盘钩子失败".into()));
                {
                    let mut guard = storage.lock().unwrap();
                    *guard = None;
                }
                drop(Box::from_raw(ctx_ptr));
                return;
            }

            let deadline = Instant::now() + timeout;
            let mut msg = MSG::default();
            while Instant::now() < deadline {
                while PeekMessageW(&mut msg, HWND(0), 0, 0, PM_REMOVE).into() {
                    if msg.message == WM_QUIT {
                        break;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if msg.message == WM_QUIT {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }

            UnhookWindowsHookEx(hook);
            {
                let mut guard = storage.lock().unwrap();
                *guard = None;
            }
            drop(Box::from_raw(ctx_ptr));
        });

        let outcome = match rx.recv_timeout(timeout + Duration::from_millis(50)) {
            Ok(HookEvent::Fn {
                latency,
                raw_ns,
                reaction,
                device_origin,
            }) => {
                let sla = Duration::from_millis(SLA_MS);
                let within_sla = reaction <= sla;
                NativeProbeObservation {
                    supported: true,
                    latency: Some(latency),
                    raw_latency_ns: Some(raw_ns),
                    user_reaction: Some(reaction),
                    within_sla: Some(within_sla),
                    device_origin,
                    interface: "SetWindowsHookEx",
                    reason: if within_sla {
                        None
                    } else {
                        Some(format!(
                            "Fn 驱动回调耗时 {}ms，超出 {}ms SLA",
                            reaction.as_millis(),
                            sla.as_millis()
                        ))
                    },
                }
            }
            Ok(HookEvent::Other(label)) => NativeProbeObservation {
                supported: false,
                latency: None,
                raw_latency_ns: None,
                user_reaction: None,
                within_sla: None,
                device_origin: None,
                interface: "SetWindowsHookEx",
                reason: Some(format!("检测到按键 {label}，未捕获到 Fn")),
            },
            Ok(HookEvent::Error(reason)) => {
                return Err(ProbeError::Io(reason));
            }
            Err(_) => {
                return Err(ProbeError::Timeout);
            }
        };

        let _ = handle.join();
        Ok(outcome)
    }

    fn probe_with_raw_input(timeout: Duration) -> Result<NativeProbeObservation, ProbeError> {
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || unsafe {
            let mut freq = 0i64;
            QueryPerformanceFrequency(&mut freq);
            let mut start_counter = 0i64;
            QueryPerformanceCounter(&mut start_counter);
            let start_tick = GetTickCount64();

            let context = Box::new(RawContext {
                sender: tx.clone(),
                start_tick,
                start_counter,
                frequency: freq,
            });
            let ctx_ptr = Box::into_raw(context);
            let storage = RAW_CONTEXT.get_or_init(|| Mutex::new(None));
            {
                let mut guard = storage.lock().unwrap();
                *guard = Some(ctx_ptr);
            }

            let module = GetModuleHandleW(None).unwrap_or(HINSTANCE::default());
            let class = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(raw_wnd_proc),
                hInstance: module,
                lpszClassName: w!("FlowwisperRawInput"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                if GetLastError() != ERROR_CLASS_ALREADY_EXISTS.0 {
                    let _ = (*ctx_ptr)
                        .sender
                        .send(RawEvent::Error("注册 Raw Input 窗口类失败".into()));
                    {
                        let mut guard = storage.lock().unwrap();
                        *guard = None;
                    }
                    drop(Box::from_raw(ctx_ptr));
                    return;
                }
            }

            let hwnd = CreateWindowExW(
                Default::default(),
                w!("FlowwisperRawInput"),
                w!("FlowwisperRawInput"),
                Default::default(),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                module,
                None,
            );
            if hwnd.0 == 0 {
                let _ = (*ctx_ptr)
                    .sender
                    .send(RawEvent::Error("创建 Raw Input 窗口失败".into()));
                {
                    let mut guard = storage.lock().unwrap();
                    *guard = None;
                }
                drop(Box::from_raw(ctx_ptr));
                return;
            }

            let device = RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x06,
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: hwnd,
            };
            if !RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32).as_bool() {
                let _ = (*ctx_ptr)
                    .sender
                    .send(RawEvent::Error("注册 Raw Input 设备失败".into()));
                DestroyWindow(hwnd);
                {
                    let mut guard = storage.lock().unwrap();
                    *guard = None;
                }
                drop(Box::from_raw(ctx_ptr));
                return;
            }

            let deadline = Instant::now() + timeout;
            let mut msg = MSG::default();
            while Instant::now() < deadline {
                while PeekMessageW(&mut msg, HWND(0), 0, 0, PM_REMOVE).into() {
                    if msg.message == WM_QUIT {
                        break;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if msg.message == WM_QUIT {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }

            DestroyWindow(hwnd);
            {
                let mut guard = storage.lock().unwrap();
                *guard = None;
            }
            drop(Box::from_raw(ctx_ptr));
        });

        let outcome = match rx.recv_timeout(timeout + Duration::from_millis(50)) {
            Ok(RawEvent::Fn {
                latency,
                raw_ns,
                reaction,
                device_origin,
            }) => {
                let sla = Duration::from_millis(SLA_MS);
                let within_sla = reaction <= sla;
                NativeProbeObservation {
                    supported: true,
                    latency: Some(latency),
                    raw_latency_ns: Some(raw_ns),
                    user_reaction: Some(reaction),
                    within_sla: Some(within_sla),
                    device_origin,
                    interface: "RawInput",
                    reason: if within_sla {
                        None
                    } else {
                        Some(format!(
                            "Fn 驱动回调耗时 {}ms，超出 {}ms SLA",
                            reaction.as_millis(),
                            sla.as_millis()
                        ))
                    },
                }
            }
            Ok(RawEvent::Other(label)) => NativeProbeObservation {
                supported: false,
                latency: None,
                raw_latency_ns: None,
                user_reaction: None,
                within_sla: None,
                device_origin: None,
                interface: "RawInput",
                reason: Some(format!("检测到按键 {label}，未捕获到 Fn")),
            },
            Ok(RawEvent::Error(reason)) => {
                return Err(ProbeError::Io(reason));
            }
            Err(_) => {
                return Err(ProbeError::Timeout);
            }
        };

        let _ = handle.join();
        Ok(outcome)
    }

    fn compute_timings(
        start_tick: u64,
        start_counter: i64,
        frequency: i64,
        event_time: u32,
    ) -> (Duration, u128, Duration) {
        let start_low = start_tick as u32;
        let reaction_ms = event_time.wrapping_sub(start_low) as u64;
        let reaction = Duration::from_millis(reaction_ms);
        let event_tick = start_tick + reaction_ms;
        let mut now_counter = 0i64;
        unsafe {
            QueryPerformanceCounter(&mut now_counter);
        }
        let reaction_counts = ((reaction_ms as i128) * (frequency as i128)) / 1000;
        let event_counter = start_counter.saturating_add(reaction_counts as i64);
        let delta_counts = now_counter.saturating_sub(event_counter);
        let raw_ns = if frequency > 0 {
            (delta_counts as u128 * 1_000_000_000u128) / frequency as u128
        } else {
            0
        };
        let latency = Duration::from_nanos(raw_ns.min(u64::MAX as u128) as u64);
        (latency, raw_ns, reaction)
    }

    fn describe_key(vk: u32) -> String {
        match vk {
            0x1B => "Esc".into(),
            0x09 => "Tab".into(),
            0x0D => "Enter".into(),
            0x08 => "Backspace".into(),
            0x20 => "Space".into(),
            0x2E => "Delete".into(),
            0x2D => "Insert".into(),
            0x25 => "Left".into(),
            0x26 => "Up".into(),
            0x27 => "Right".into(),
            0x28 => "Down".into(),
            0x70..=0x7B => format!("F{}", vk - 0x6F),
            _ => format!("VK(0x{vk:X})"),
        }
    }

    fn is_fn_key(kb: &KBDLLHOOKSTRUCT) -> bool {
        kb.vkCode == 0xFF || (kb.vkCode == 0 && kb.scanCode == 0)
    }

    fn is_raw_fn_key(kb: &RAWKEYBOARD) -> bool {
        kb.VKey == 0 || kb.VKey == 0xFF || (kb.VKey == 255 && kb.MakeCode == 0)
    }

    fn raw_device_origin(handle: windows::Win32::Foundation::HANDLE) -> Option<String> {
        unsafe {
            let mut size = 0u32;
            if GetRawInputDeviceInfoW(handle, RIDI_DEVICENAME, None, &mut size) == u32::MAX
                || size == 0
            {
                return None;
            }
            let mut buffer = vec![0u16; size as usize];
            let result = GetRawInputDeviceInfoW(
                handle,
                RIDI_DEVICENAME,
                Some(buffer.as_mut_ptr() as *mut c_void),
                &mut size,
            );
            if result == u32::MAX || result == 0 {
                return None;
            }
            let mut label = String::from_utf16_lossy(&buffer[..result as usize]);
            if GetSystemMetrics(SM_REMOTESESSION) != 0 {
                label = format!("RemoteSession/{label}");
            }
            Some(label)
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use super::{NativeProbeObservation, ProbeError};
    use std::time::Duration;

    pub fn probe_fn(timeout: Duration) -> Result<NativeProbeObservation, ProbeError> {
        let _ = timeout;
        Err(ProbeError::Io("当前平台未实现原生 Fn 探测".into()))
    }
}

pub fn probe_fn(timeout: Duration) -> Result<NativeProbeObservation, ProbeError> {
    platform::probe_fn(timeout)
}
