use leptos_ui_theme_core::{ProjectConfig, ResolvedProfile};

#[must_use]
pub fn seeded_module() -> String {
    r#"pub mod controller;
pub mod generated;
pub mod scope;

pub use controller::{
    ThemeController, ThemePreference, RuntimeIssue, provide_theme_controller,
    use_theme_controller,
};
pub use generated::{ThemeId, ThemeMetadata, THEMES, THEME_IDS, parse_theme_id};
pub use scope::{ThemeScope, ThemeScopeContext, use_theme_scope};
"#
    .to_owned()
}

#[must_use]
pub fn seeded_controller(_config: &ProjectConfig, _profiles: &[ResolvedProfile]) -> String {
    r#"//! Application-owned theme preference and browser integration.

use super::generated::{
    BOOTSTRAP_ATTRIBUTE, BOOTSTRAP_ENABLED, BOOTSTRAP_OUTCOME_PROPERTY, STORAGE_KEY,
    SYSTEM_DARK_THEME, SYSTEM_LIGHT_THEME, THEME_ATTRIBUTE, ThemeId, parse_theme_id,
};
use leptos::prelude::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThemePreference {
    System,
    Named(ThemeId),
}

impl ThemePreference {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            value => parse_theme_id(value).map(Self::Named),
        }
    }

    #[must_use]
    pub const fn storage_value(self) -> Option<&'static str> {
        match self {
            Self::System => None,
            Self::Named(theme) => Some(theme.as_str()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuntimeIssue {
    BootstrapOutcomeUnavailable,
    BootstrapStorageUnavailable,
    BootstrapStorageReadFailed,
    BootstrapDomApplyFailed,
    StorageReadFailed,
    StorageWriteFailed,
    DomApplyFailed,
}

impl RuntimeIssue {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BootstrapOutcomeUnavailable => "bootstrap-outcome-unavailable",
            Self::BootstrapStorageUnavailable => "bootstrap-storage-unavailable",
            Self::BootstrapStorageReadFailed => "bootstrap-storage-read-failed",
            Self::BootstrapDomApplyFailed => "bootstrap-dom-apply-failed",
            Self::StorageReadFailed => "storage-read-failed",
            Self::StorageWriteFailed => "storage-write-failed",
            Self::DomApplyFailed => "dom-apply-failed",
        }
    }
}

#[derive(Clone, Copy)]
pub struct ThemeController {
    preference: RwSignal<ThemePreference>,
    effective_theme: RwSignal<ThemeId>,
    latest_issue: RwSignal<Option<RuntimeIssue>>,
}

impl ThemeController {
    #[must_use]
    pub fn preference(self) -> ReadSignal<ThemePreference> {
        self.preference.read_only()
    }

    #[must_use]
    pub fn effective_theme(self) -> ReadSignal<ThemeId> {
        self.effective_theme.read_only()
    }

    #[must_use]
    pub fn latest_issue(self) -> ReadSignal<Option<RuntimeIssue>> {
        self.latest_issue.read_only()
    }

    pub fn set_preference(self, preference: ThemePreference) {
        self.preference.set(preference);
        if let Err(issue) = browser::apply(preference) {
            self.latest_issue.set(Some(issue));
        }
        if let Err(issue) = browser::persist(preference) {
            self.latest_issue.set(Some(issue));
        }
        self.refresh_effective();
    }

    fn update_without_persistence(self, preference: ThemePreference) {
        self.preference.set(preference);
        if let Err(issue) = browser::apply(preference) {
            self.latest_issue.set(Some(issue));
        }
        self.refresh_effective();
    }

    fn refresh_effective(self) {
        let preference = self.preference.get_untracked();
        self.effective_theme.set(match preference {
            ThemePreference::Named(theme) => theme,
            ThemePreference::System => browser::system_theme(),
        });
    }
}

pub fn provide_theme_controller() -> ThemeController {
    let (preference, issues) = browser::initialize();
    let controller = ThemeController {
        preference: RwSignal::new(preference),
        effective_theme: RwSignal::new(match preference {
            ThemePreference::Named(theme) => theme,
            ThemePreference::System => browser::system_theme(),
        }),
        latest_issue: RwSignal::new(issues.last().copied()),
    };
    provide_context(controller);
    browser::install_listeners(controller);
    controller
}

#[must_use]
pub fn use_theme_controller() -> Option<ThemeController> {
    use_context()
}

#[cfg(not(target_arch = "wasm32"))]
mod browser {
    use super::*;

    pub fn initialize() -> (ThemePreference, Vec<RuntimeIssue>) {
        (ThemePreference::System, Vec::new())
    }

    pub fn apply(_preference: ThemePreference) -> Result<(), RuntimeIssue> {
        Ok(())
    }

    pub fn persist(_preference: ThemePreference) -> Result<(), RuntimeIssue> {
        Ok(())
    }

    pub fn system_theme() -> ThemeId {
        SYSTEM_LIGHT_THEME
    }

    pub fn install_listeners(_controller: ThemeController) {}
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::*;
    use leptos::__reexports::send_wrapper::SendWrapper;
    use leptos::wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
    use leptos::web_sys::{Event, Storage, StorageEvent, window};

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = Object, js_name = getOwnPropertyDescriptor)]
        fn own_property_descriptor(object: &JsValue, key: &str) -> JsValue;

        #[wasm_bindgen(js_namespace = Reflect, js_name = deleteProperty)]
        fn delete_property(object: &JsValue, key: &str) -> Result<bool, JsValue>;

        #[wasm_bindgen(method, getter, structural)]
        fn value(this: &JsValue) -> JsValue;

        #[wasm_bindgen(method, getter, structural)]
        fn enumerable(this: &JsValue) -> bool;

        #[wasm_bindgen(method, getter, structural)]
        fn writable(this: &JsValue) -> bool;

        #[wasm_bindgen(method, getter, structural)]
        fn configurable(this: &JsValue) -> bool;
    }

    pub fn initialize() -> (ThemePreference, Vec<RuntimeIssue>) {
        let mut issues = Vec::new();
        if BOOTSTRAP_ENABLED {
            if let Some(preference) = adopt_bootstrap(&mut issues) {
                remove_bootstrap_marker();
                return (preference, issues);
            }
        } else {
            discard_stale_transfer();
        }
        remove_bootstrap_marker();
        let preference = read_storage(&mut issues);
        if let Err(issue) = apply(preference) {
            issues.push(issue);
        }
        (preference, issues)
    }

    fn adopt_bootstrap(issues: &mut Vec<RuntimeIssue>) -> Option<ThemePreference> {
        let global = window()?;
        let descriptor = own_property_descriptor(global.as_ref(), BOOTSTRAP_OUTCOME_PROPERTY);
        if descriptor.is_undefined()
            || descriptor.is_null()
            || descriptor.enumerable()
            || descriptor.writable()
            || !descriptor.configurable()
        {
            issues.push(RuntimeIssue::BootstrapOutcomeUnavailable);
            return None;
        }
        let Some(outcome) = descriptor.value().as_string() else {
            issues.push(RuntimeIssue::BootstrapOutcomeUnavailable);
            return None;
        };
        let parsed_outcome = match outcome.as_str() {
            "v1:ok:ok" => Some((None, true)),
            "v1:unavailable:ok" => {
                Some((Some(RuntimeIssue::BootstrapStorageUnavailable), true))
            }
            "v1:read-failed:ok" => {
                Some((Some(RuntimeIssue::BootstrapStorageReadFailed), true))
            }
            "v1:ok:apply-failed" => {
                Some((Some(RuntimeIssue::BootstrapDomApplyFailed), false))
            }
            "v1:unavailable:apply-failed" => {
                issues.push(RuntimeIssue::BootstrapStorageUnavailable);
                Some((Some(RuntimeIssue::BootstrapDomApplyFailed), false))
            }
            "v1:read-failed:apply-failed" => {
                issues.push(RuntimeIssue::BootstrapStorageReadFailed);
                Some((Some(RuntimeIssue::BootstrapDomApplyFailed), false))
            }
            _ => None,
        };
        let Some((issue, dom_ok)) = parsed_outcome else {
            issues.push(RuntimeIssue::BootstrapOutcomeUnavailable);
            return None;
        };
        if delete_property(global.as_ref(), BOOTSTRAP_OUTCOME_PROPERTY) != Ok(true) {
            issues.push(RuntimeIssue::BootstrapOutcomeUnavailable);
            return None;
        }
        if let Some(issue) = issue {
            issues.push(issue);
        }
        if !dom_ok {
            return None;
        }
        let root = global.document()?.document_element()?;
        let marker = root.get_attribute(BOOTSTRAP_ATTRIBUTE)?;
        let raw = marker.strip_prefix("v1:")?;
        let preference = ThemePreference::parse(raw)?;
        let matches = match preference {
            ThemePreference::System => !root.has_attribute(THEME_ATTRIBUTE),
            ThemePreference::Named(theme) => {
                root.get_attribute(THEME_ATTRIBUTE).as_deref() == Some(theme.as_str())
            }
        };
        matches.then_some(preference)
    }

    fn discard_stale_transfer() {
        let Some(global) = window() else {
            return;
        };
        let descriptor = own_property_descriptor(global.as_ref(), BOOTSTRAP_OUTCOME_PROPERTY);
        if !descriptor.is_null() && !descriptor.is_undefined() && descriptor.configurable() {
            let _ = delete_property(global.as_ref(), BOOTSTRAP_OUTCOME_PROPERTY);
        }
    }

    fn remove_bootstrap_marker() {
        if let Some(root) = window()
            .and_then(|window| window.document())
            .and_then(|document| document.document_element())
        {
            let _ = root.remove_attribute(BOOTSTRAP_ATTRIBUTE);
        }
    }

    fn local_storage() -> Result<Option<Storage>, RuntimeIssue> {
        window()
            .ok_or(RuntimeIssue::StorageReadFailed)?
            .local_storage()
            .map_err(|_| RuntimeIssue::StorageReadFailed)
    }

    fn read_storage(issues: &mut Vec<RuntimeIssue>) -> ThemePreference {
        match local_storage().and_then(|storage| {
            storage
                .map(|storage| storage.get_item(STORAGE_KEY))
                .transpose()
                .map_err(|_| RuntimeIssue::StorageReadFailed)
        }) {
            Ok(Some(Some(value))) => {
                ThemePreference::parse(&value).unwrap_or(ThemePreference::System)
            }
            Ok(_) => ThemePreference::System,
            Err(issue) => {
                issues.push(issue);
                ThemePreference::System
            }
        }
    }

    pub fn apply(preference: ThemePreference) -> Result<(), RuntimeIssue> {
        let root = window()
            .and_then(|window| window.document())
            .and_then(|document| document.document_element())
            .ok_or(RuntimeIssue::DomApplyFailed)?;
        match preference {
            ThemePreference::System => root.remove_attribute(THEME_ATTRIBUTE),
            ThemePreference::Named(theme) => {
                root.set_attribute(THEME_ATTRIBUTE, theme.as_str())
            }
        }
        .map_err(|_| RuntimeIssue::DomApplyFailed)
    }

    pub fn persist(preference: ThemePreference) -> Result<(), RuntimeIssue> {
        let storage = window()
            .ok_or(RuntimeIssue::StorageWriteFailed)?
            .local_storage()
            .map_err(|_| RuntimeIssue::StorageWriteFailed)?
            .ok_or(RuntimeIssue::StorageWriteFailed)?;
        match preference.storage_value() {
            Some(value) => storage.set_item(STORAGE_KEY, value),
            None => storage.remove_item(STORAGE_KEY),
        }
        .map_err(|_| RuntimeIssue::StorageWriteFailed)
    }

    pub fn system_theme() -> ThemeId {
        let dark = window()
            .and_then(|window| window.match_media("(prefers-color-scheme: dark)").ok().flatten())
            .is_some_and(|query| query.matches());
        if dark {
            SYSTEM_DARK_THEME
        } else {
            SYSTEM_LIGHT_THEME
        }
    }

    pub fn install_listeners(controller: ThemeController) {
        let Some(global) = window() else {
            return;
        };
        let storage = global.local_storage().ok().flatten();
        let listener = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Ok(event) = event.dyn_into::<StorageEvent>() else {
                return;
            };
            if event.storage_area() != storage {
                return;
            }
            let preference = match event.key() {
                None => ThemePreference::System,
                Some(key) if key == STORAGE_KEY => event
                    .new_value()
                    .as_deref()
                    .and_then(ThemePreference::parse)
                    .unwrap_or(ThemePreference::System),
                Some(_) => return,
            };
            if controller.preference.get_untracked() != preference {
                controller.update_without_persistence(preference);
            }
        });
        if global
            .add_event_listener_with_callback("storage", listener.as_ref().unchecked_ref())
            .is_err()
        {
            return;
        }
        let cleanup = SendWrapper::new((global, listener));
        on_cleanup(move || {
            let (global, listener) = cleanup.take();
            let _ = global.remove_event_listener_with_callback(
                "storage",
                listener.as_ref().unchecked_ref(),
            );
        });
    }
}
"#
    .to_owned()
}

#[must_use]
pub fn seeded_scope(config: &ProjectConfig) -> String {
    r#"//! Application-owned nested theme scopes and direct-body portal hosts.

use super::generated::{THEME_ATTRIBUTE, ThemeId};
use leptos::prelude::*;
use web_ui_primitives::leptos::PortalMount;

#[derive(Clone)]
pub struct ThemeScopeContext {
    pub theme: Signal<Option<ThemeId>>,
    pub portal_mount: Option<PortalMount>,
}

#[must_use]
pub fn use_theme_scope() -> Option<ThemeScopeContext> {
    use_context()
}

#[component]
pub fn ThemeScope(
    #[prop(into)] theme: Signal<Option<ThemeId>>,
    #[prop(optional)] provide_portal_host: bool,
    children: Children,
) -> impl IntoView {
    let portal_mount = create_portal_mount(provide_portal_host);
    provide_context(ThemeScopeContext {
        theme,
        portal_mount: portal_mount.clone(),
    });
    sync_portal_theme(portal_mount);
    view! {
        <div attr:data-ui-theme=move || theme.get().map(ThemeId::as_str)>
            {children()}
        </div>
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn create_portal_mount(_enabled: bool) -> Option<PortalMount> {
    None
}

#[cfg(target_arch = "wasm32")]
fn create_portal_mount(enabled: bool) -> Option<PortalMount> {
    use leptos::__reexports::send_wrapper::SendWrapper;
    let document = enabled
        .then(leptos::web_sys::window)
        .flatten()?
        .document()?;
    let body = document.body()?;
    let host = document.create_element("div").ok()?;
    body.append_child(&host).ok()?;
    let cleanup = SendWrapper::new((body, host.clone()));
    on_cleanup(move || {
        let (body, host) = cleanup.take();
        let _ = body.remove_child(&host);
    });
    Some(host)
}

#[cfg(not(target_arch = "wasm32"))]
fn sync_portal_theme(_portal_mount: Option<PortalMount>) {}

#[cfg(target_arch = "wasm32")]
fn sync_portal_theme(portal_mount: Option<PortalMount>) {
    let Some(host) = portal_mount else {
        return;
    };
    let Some(scope) = use_theme_scope() else {
        return;
    };
    Effect::new(move |_| match scope.theme.get() {
        Some(theme) => {
            let _ = host.set_attribute(THEME_ATTRIBUTE, theme.as_str());
        }
        None => {
            let _ = host.remove_attribute(THEME_ATTRIBUTE);
        }
    });
}
"#
    .replace("data-ui-theme", &config.selectors.theme)
}
