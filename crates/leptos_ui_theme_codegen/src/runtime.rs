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
pub use generated::{
    DEFAULT_THEME, SYSTEM_DARK_THEME, SYSTEM_LIGHT_THEME, THEMES, THEME_IDS,
    ThemeColorScheme, ThemeId, ThemeMetadata, parse_theme_id,
};
pub use scope::{ThemeScope, ThemeScopeContext, use_theme_scope};
"#
    .to_owned()
}

#[must_use]
pub fn seeded_controller(_config: &ProjectConfig, _profiles: &[ResolvedProfile]) -> String {
    r#"//! Application-owned theme preference and browser integration.

use super::generated::{
    SYSTEM_LIGHT_THEME, ThemeId, parse_theme_id,
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
        let apply_issue = browser::apply(preference).err();
        let persist_issue = browser::persist(preference).err();
        self.latest_issue.set(persist_issue.or(apply_issue));
        self.refresh_effective();
    }

    #[cfg(target_arch = "wasm32")]
    fn update_without_persistence(self, preference: ThemePreference) {
        self.preference.set(preference);
        self.latest_issue.set(browser::apply(preference).err());
        self.refresh_effective();
    }

    #[cfg(target_arch = "wasm32")]
    fn adopt_after_hydration(
        self,
        preference: ThemePreference,
        latest_issue: Option<RuntimeIssue>,
    ) {
        self.preference.set(preference);
        self.latest_issue.set(latest_issue);
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
    let preference = ThemePreference::System;
    let controller = ThemeController {
        preference: RwSignal::new(preference),
        effective_theme: RwSignal::new(SYSTEM_LIGHT_THEME),
        latest_issue: RwSignal::new(None),
    };
    provide_context(controller);
    browser::install_after_hydration(controller);
    controller
}

#[must_use]
pub fn use_theme_controller() -> Option<ThemeController> {
    use_context()
}

#[cfg(not(target_arch = "wasm32"))]
mod browser {
    use super::*;

    pub fn apply(_preference: ThemePreference) -> Result<(), RuntimeIssue> {
        Ok(())
    }

    pub fn persist(_preference: ThemePreference) -> Result<(), RuntimeIssue> {
        Ok(())
    }

    pub fn system_theme() -> ThemeId {
        SYSTEM_LIGHT_THEME
    }

    pub fn install_after_hydration(_controller: ThemeController) {}
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::*;
    use super::super::generated::{
        BOOTSTRAP_ATTRIBUTE, BOOTSTRAP_ENABLED, BOOTSTRAP_OUTCOME_PROPERTY, STORAGE_KEY,
        SYSTEM_DARK_THEME, THEME_ATTRIBUTE,
    };
    use leptos::wasm_bindgen::{JsCast, JsValue, closure::Closure};
    use leptos::web_sys::{Event, js_sys, window};

    fn initialize() -> (ThemePreference, Vec<RuntimeIssue>) {
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
        let descriptor = own_property_descriptor(global.as_ref())?;
        if descriptor.is_undefined()
            || descriptor.is_null()
            || descriptor_flag(&descriptor, "enumerable") != Some(false)
            || descriptor_flag(&descriptor, "writable") != Some(false)
            || descriptor_flag(&descriptor, "configurable") != Some(true)
            || !descriptor_has_value(&descriptor)
        {
            issues.push(RuntimeIssue::BootstrapOutcomeUnavailable);
            return None;
        }
        let Some(outcome) = property(&descriptor, "value").ok()?.as_string() else {
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
        if delete_transfer(global.as_ref()) != Ok(true) {
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
        let Some(descriptor) = own_property_descriptor(global.as_ref()) else {
            return;
        };
        if !descriptor.is_null()
            && !descriptor.is_undefined()
            && descriptor_flag(&descriptor, "configurable") == Some(true)
        {
            let _ = delete_transfer(global.as_ref());
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

    fn own_property_descriptor(global: &JsValue) -> Option<JsValue> {
        let object: &js_sys::Object = global.unchecked_ref();
        js_sys::Reflect::get_own_property_descriptor(
            object,
            &JsValue::from_str(BOOTSTRAP_OUTCOME_PROPERTY),
        )
        .ok()
    }

    fn descriptor_flag(descriptor: &JsValue, key: &str) -> Option<bool> {
        property(descriptor, key).ok()?.as_bool()
    }

    fn descriptor_has_value(descriptor: &JsValue) -> bool {
        let object: &js_sys::Object = descriptor.unchecked_ref();
        js_sys::Object::has_own(object, &JsValue::from_str("value"))
    }

    fn delete_transfer(global: &JsValue) -> Result<bool, JsValue> {
        let object: &js_sys::Object = global.unchecked_ref();
        js_sys::Reflect::delete_property(
            object,
            &JsValue::from_str(BOOTSTRAP_OUTCOME_PROPERTY),
        )
    }

    fn property(object: &JsValue, key: &str) -> Result<JsValue, JsValue> {
        js_sys::Reflect::get(object, &JsValue::from_str(key))
    }

    fn call_method(
        object: &JsValue,
        method: &str,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsValue> {
        let function = property(object, method)?.dyn_into::<js_sys::Function>()?;
        match arguments {
            [] => function.call0(object),
            [first] => function.call1(object, first),
            [first, second] => function.call2(object, first, second),
            _ => Err(JsValue::from_str("unsupported browser adapter arity")),
        }
    }

    fn local_storage(issue: RuntimeIssue) -> Result<Option<JsValue>, RuntimeIssue> {
        let global = window().ok_or(issue)?;
        let storage = property(global.as_ref(), "localStorage").map_err(|_| issue)?;
        Ok((!storage.is_null() && !storage.is_undefined()).then_some(storage))
    }

    fn read_storage(issues: &mut Vec<RuntimeIssue>) -> ThemePreference {
        match local_storage(RuntimeIssue::StorageReadFailed).and_then(|storage| {
            storage
                .map(|storage| {
                    call_method(
                        &storage,
                        "getItem",
                        &[JsValue::from_str(STORAGE_KEY)],
                    )
                    .map(|value| (!value.is_null()).then(|| value.as_string()).flatten())
                })
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
            ThemePreference::System if root.has_attribute(THEME_ATTRIBUTE) => {
                root.remove_attribute(THEME_ATTRIBUTE)
            }
            ThemePreference::Named(theme)
                if root.get_attribute(THEME_ATTRIBUTE).as_deref()
                    != Some(theme.as_str()) =>
            {
                root.set_attribute(THEME_ATTRIBUTE, theme.as_str())
            }
            ThemePreference::System | ThemePreference::Named(_) => Ok(()),
        }
        .map_err(|_| RuntimeIssue::DomApplyFailed)
    }

    pub fn persist(preference: ThemePreference) -> Result<(), RuntimeIssue> {
        let storage = local_storage(RuntimeIssue::StorageWriteFailed)?
            .ok_or(RuntimeIssue::StorageWriteFailed)?;
        let result = match preference.storage_value() {
            Some(value) => call_method(
                &storage,
                "setItem",
                &[JsValue::from_str(STORAGE_KEY), JsValue::from_str(value)],
            ),
            None => call_method(
                &storage,
                "removeItem",
                &[JsValue::from_str(STORAGE_KEY)],
            ),
        };
        result
            .map(|_| ())
            .map_err(|_| RuntimeIssue::StorageWriteFailed)
    }

    pub fn system_theme() -> ThemeId {
        let dark = window()
            .and_then(|window| {
                call_method(
                    window.as_ref(),
                    "matchMedia",
                    &[JsValue::from_str("(prefers-color-scheme: dark)")],
                )
                .ok()
            })
            .and_then(|query| property(&query, "matches").ok())
            .and_then(|matches| matches.as_bool())
            .unwrap_or(false);
        if dark {
            SYSTEM_DARK_THEME
        } else {
            SYSTEM_LIGHT_THEME
        }
    }

    pub fn install_after_hydration(controller: ThemeController) {
        Effect::new(move |_| {
            let (preference, issues) = initialize();
            controller.adopt_after_hydration(preference, issues.last().copied());
            install_listeners(controller);
        });
    }

    fn install_listeners(controller: ThemeController) {
        let Some(global) = window() else {
            return;
        };
        let storage_listener = local_storage(RuntimeIssue::StorageReadFailed)
            .ok()
            .flatten()
            .and_then(|storage| {
                let listener = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
                    let event = event.as_ref();
                    let Ok(event_storage) = property(event, "storageArea") else {
                        return;
                    };
                    if !js_sys::Object::is(&event_storage, &storage) {
                        return;
                    }
                    let key = property(event, "key").ok();
                    let preference = match key.as_ref().and_then(JsValue::as_string) {
                        None if key.as_ref().is_some_and(JsValue::is_null) => {
                            ThemePreference::System
                        }
                        Some(key) if key == STORAGE_KEY => property(event, "newValue")
                            .ok()
                            .as_ref()
                            .and_then(JsValue::as_string)
                            .as_deref()
                            .and_then(ThemePreference::parse)
                            .unwrap_or(ThemePreference::System),
                        _ => return,
                    };
                    if controller.preference.get_untracked() != preference {
                        controller.update_without_persistence(preference);
                    }
                });
                global
                    .add_event_listener_with_callback(
                        "storage",
                        listener.as_ref().unchecked_ref(),
                    )
                    .ok()
                    .map(|_| listener)
            });
        let media_listener = call_method(
            global.as_ref(),
            "matchMedia",
            &[JsValue::from_str("(prefers-color-scheme: dark)")],
        )
        .ok()
        .and_then(|query| {
            let listener = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
                if controller.preference.get_untracked() == ThemePreference::System {
                    controller.refresh_effective();
                }
            });
            call_method(
                &query,
                "addEventListener",
                &[JsValue::from_str("change"), listener.as_ref().clone()],
            )
            .ok()
            .map(|_| (query, listener))
        });
        if storage_listener.is_none() && media_listener.is_none() {
            return;
        }
        let cleanup = StoredValue::new_local((global, storage_listener, media_listener));
        on_cleanup(move || {
            let Some((global, storage_listener, media_listener)) = cleanup.into_inner() else {
                return;
            };
            if let Some(listener) = storage_listener {
                let _ = global.remove_event_listener_with_callback(
                    "storage",
                    listener.as_ref().unchecked_ref(),
                );
            }
            if let Some((query, listener)) = media_listener {
                let _ = call_method(
                    &query,
                    "removeEventListener",
                    &[JsValue::from_str("change"), listener.as_ref().clone()],
                );
            }
        });
    }
}
"#
    .to_owned()
}

#[must_use]
pub fn seeded_scope(config: &ProjectConfig) -> String {
    r#"//! Application-owned nested theme scopes and direct-body portal hosts.

use super::generated::ThemeId;
use leptos::prelude::*;
use web_ui_primitives::leptos::PortalMount;

#[derive(Clone)]
pub struct ThemeScopeContext {
    pub effective_theme: Signal<Option<ThemeId>>,
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
    let parent = use_theme_scope();
    let parent_theme = parent.as_ref().map(|scope| scope.effective_theme);
    let effective_theme = Signal::derive(move || {
        theme
            .get()
            .or_else(|| parent_theme.and_then(|signal| signal.get()))
    });
    let portal_mount = create_portal_mount(provide_portal_host).or_else(|| {
        parent
            .as_ref()
            .and_then(|scope| scope.portal_mount.clone())
    });
    provide_context(ThemeScopeContext {
        effective_theme,
        portal_mount: portal_mount.clone(),
    });
    sync_portal_theme(portal_mount, effective_theme);
    view! {
        <div attr:data-ui-theme=move || effective_theme.get().map(ThemeId::as_str)>
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
    let document = enabled
        .then(leptos::web_sys::window)
        .flatten()?
        .document()?;
    let body = document.body()?;
    let host = document.create_element("div").ok()?;
    host.set_attribute("data-leptos-ui-theme-portal-host", "")
        .ok()?;
    body.append_child(&host).ok()?;
    let cleanup = StoredValue::new_local((body, host.clone()));
    on_cleanup(move || {
        // Leptos disposes child owners before parent cleanup callbacks. A
        // web_ui_primitives Portal therefore drops its mount handle and
        // removes portaled content before this scope-owned host is removed.
        if let Some((body, host)) = cleanup.into_inner() {
            let _ = body.remove_child(&host);
        }
    });
    Some(host)
}

#[cfg(not(target_arch = "wasm32"))]
fn sync_portal_theme(
    _portal_mount: Option<PortalMount>,
    _effective_theme: Signal<Option<ThemeId>>,
) {
}

#[cfg(target_arch = "wasm32")]
fn sync_portal_theme(
    portal_mount: Option<PortalMount>,
    effective_theme: Signal<Option<ThemeId>>,
) {
    use super::generated::THEME_ATTRIBUTE;
    let Some(host) = portal_mount else {
        return;
    };
    Effect::new(move |_| match effective_theme.get() {
        Some(theme)
            if host.get_attribute(THEME_ATTRIBUTE).as_deref()
                != Some(theme.as_str()) =>
        {
            let _ = host.set_attribute(THEME_ATTRIBUTE, theme.as_str());
        }
        None if host.has_attribute(THEME_ATTRIBUTE) => {
            let _ = host.remove_attribute(THEME_ATTRIBUTE);
        }
        Some(_) | None => {}
    });
}
"#
    .replace("data-ui-theme", &config.selectors.theme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use leptos_ui_theme_core::ProjectConfig;

    #[test]
    fn generated_runtime_uses_only_public_leptos_apis() {
        let config = ProjectConfig::default();
        let controller = seeded_controller(&config, &[]);
        let scope = seeded_scope(&config);

        assert!(!controller.contains("leptos::__reexports"));
        assert!(!scope.contains("leptos::__reexports"));
        assert!(controller.contains("StoredValue::new_local"));
        assert!(scope.contains("StoredValue::new_local"));
    }

    #[test]
    fn generated_scope_propagates_effective_identity_and_portal_mount() {
        let scope = seeded_scope(&ProjectConfig::default());

        assert!(scope.contains("pub effective_theme: Signal<Option<ThemeId>>"));
        assert!(scope.contains("parent_theme.and_then(|signal| signal.get())"));
        assert!(scope.contains("scope.portal_mount.clone()"));
        assert!(scope.contains("cleanup.into_inner()"));
        assert!(scope.contains("data-leptos-ui-theme-portal-host"));
        assert!(scope.contains("host.get_attribute(THEME_ATTRIBUTE)"));
    }

    #[test]
    fn generated_controller_covers_preference_adoption_and_listeners() {
        let controller = seeded_controller(&ProjectConfig::default(), &[]);

        assert!(controller.contains("pub enum ThemePreference"));
        assert!(controller.contains("pub enum RuntimeIssue"));
        assert!(controller.contains("\"storage\""));
        assert!(controller.contains("\"matchMedia\""));
        assert!(controller.contains("Effect::new(move |_|"));
        assert!(controller.contains("adopt_after_hydration"));
        assert!(controller.contains("let preference = ThemePreference::System"));
        assert!(controller.contains("controller.preference.get_untracked() != preference"));
        assert!(controller.contains("self.latest_issue.set(persist_issue.or(apply_issue))"));
        assert!(controller.contains("root.get_attribute(THEME_ATTRIBUTE)"));
        assert_eq!(
            controller
                .matches("let parsed_outcome = match outcome.as_str()")
                .count(),
            1
        );
    }
}
