use serde_json::{Value, json};
use tauri::{
    Manager, Runtime,
    plugin::{Builder, PluginHandle, TauriPlugin},
};

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("fesen")
        .setup(|app, api| {
            let plugin = api.register_android_plugin("io.fesen.bridge", "FesenPlugin")?;
            app.manage(plugin);
            Ok(())
        })
        .build()
}

pub fn request<R: Runtime>(
    app: &tauri::AppHandle<R>,
    request: Value,
    launch: bool,
) -> Result<Value, String> {
    app.state::<PluginHandle<R>>()
        .run_mobile_plugin("request", json!({ "request": request, "launch": launch }))
        .map_err(|_| String::from("not_connected"))
}

pub async fn site<R: Runtime>(app: &tauri::AppHandle<R>, request: Value) -> Result<String, String> {
    app.state::<PluginHandle<R>>()
        .run_mobile_plugin_async::<String>("siteView", request)
        .await
        .map_err(|_| "browser_failed".to_owned())
}

pub async fn add_tile<R: Runtime>(app: &tauri::AppHandle<R>) -> Result<String, String> {
    let reply = app
        .state::<PluginHandle<R>>()
        .run_mobile_plugin_async::<Value>("addTile", json!({}))
        .await
        .map_err(|_| "internal".to_owned())?;
    Ok(reply["status"]
        .as_str()
        .unwrap_or("dismissed")
        .to_owned())
}
