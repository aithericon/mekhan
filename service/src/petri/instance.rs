use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::petri::client::{PetriClient, PetriError};

/// Parameterize the compiled AIR JSON for a specific instance.
///
/// Replaces template placeholders with instance-specific values:
/// - `__INSTANCE_ID__` -> instance UUID
/// - `__TIMESTAMP__` -> current ISO 8601 timestamp
/// - `__TEMPLATE_ID__` -> template UUID
/// - Also injects system fields into initial tokens.
pub fn parameterize_air(
    air_json: &Value,
    instance_id: Uuid,
    template_id: Uuid,
    template_version: i32,
    created_by: Uuid,
    metadata: Option<&Value>,
) -> Value {
    let now = Utc::now().to_rfc3339();
    let mut air_str = serde_json::to_string(air_json).unwrap_or_default();

    air_str = air_str.replace("__INSTANCE_ID__", &instance_id.to_string());
    air_str = air_str.replace("__TIMESTAMP__", &now);
    air_str = air_str.replace("__TEMPLATE_ID__", &template_id.to_string());

    let mut air: Value = serde_json::from_str(&air_str).unwrap_or(json!({}));

    // Inject system fields into initial tokens
    if let Some(places) = air.get_mut("places").and_then(|p| p.as_array_mut()) {
        for place in places {
            if let Some(tokens) = place.get_mut("initial_tokens").and_then(|t| t.as_array_mut()) {
                for token in tokens {
                    if let Some(obj) = token.as_object_mut() {
                        obj.insert(
                            "_instance_id".to_string(),
                            json!(instance_id.to_string()),
                        );
                        obj.insert(
                            "_template_id".to_string(),
                            json!(template_id.to_string()),
                        );
                        obj.insert(
                            "_template_version".to_string(),
                            json!(template_version),
                        );
                        obj.insert("_created_at".to_string(), json!(now));
                        obj.insert(
                            "_created_by".to_string(),
                            json!(created_by.to_string()),
                        );

                        // Merge instance metadata into token
                        if let Some(meta) = metadata {
                            if let Some(meta_obj) = meta.as_object() {
                                for (k, v) in meta_obj {
                                    obj.insert(k.clone(), v.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    air
}

/// Deploy a workflow instance to petri-lab.
///
/// 1. Parameterize AIR JSON
/// 2. Deploy scenario to petri-lab
/// 3. Set run mode to "running"
pub async fn deploy_instance(
    client: &PetriClient,
    net_id: &str,
    air_json: &Value,
) -> Result<(), PetriError> {
    // Deploy the scenario
    client.deploy_scenario(net_id, air_json).await?;

    // Start execution
    client.set_run_mode(net_id, petri_api_types::RunMode::Running).await?;

    Ok(())
}
