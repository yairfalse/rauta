//! Assertion helpers for Gateway API status checking

use gateway_api::apis::standard::gateways::Gateway;
use kube::api::Api;
use kube::ResourceExt;
use serde_json::Value;

/// Assert Gateway listener has ResolvedRefs condition with expected status
pub async fn assert_listener_resolved_refs(
    client: &kube::Client,
    namespace: &str,
    gateway_name: &str,
    listener_name: &str,
    expected_status: &str,
    expected_reason: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let gateways: Api<Gateway> = Api::namespaced(client.clone(), namespace);
    let gateway = gateways.get(gateway_name).await?;

    let status = gateway
        .status
        .as_ref()
        .ok_or("Gateway has no status")?;

    let listeners = status
        .listeners
        .as_ref()
        .ok_or("Gateway status has no listeners")?;

    let listener = listeners
        .iter()
        .find(|l| l.name.as_ref() == Some(&listener_name.to_string()))
        .ok_or(format!("Listener {} not found", listener_name))?;

    // Parse listener JSON to access conditions
    let listener_json = serde_json::to_value(listener)?;
    let conditions = listener_json["conditions"]
        .as_array()
        .ok_or("Listener has no conditions")?;

    let resolved_refs = conditions
        .iter()
        .find(|c| c["type"] == "ResolvedRefs")
        .ok_or("ResolvedRefs condition not found")?;

    let status = resolved_refs["status"]
        .as_str()
        .ok_or("ResolvedRefs has no status")?;
    let reason = resolved_refs["reason"]
        .as_str()
        .ok_or("ResolvedRefs has no reason")?;

    if status != expected_status {
        return Err(format!(
            "Expected ResolvedRefs status '{}', got '{}'",
            expected_status, status
        )
        .into());
    }

    if reason != expected_reason {
        return Err(format!(
            "Expected ResolvedRefs reason '{}', got '{}'",
            expected_reason, reason
        )
        .into());
    }

    println!(
        "✅ Gateway {}/{} listener '{}': ResolvedRefs={} ({})",
        namespace, gateway_name, listener_name, status, reason
    );

    Ok(())
}

/// Assert Gateway listener ResolvedRefs message contains substring
pub async fn assert_listener_message_contains(
    client: &kube::Client,
    namespace: &str,
    gateway_name: &str,
    listener_name: &str,
    expected_substring: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let gateways: Api<Gateway> = Api::namespaced(client.clone(), namespace);
    let gateway = gateways.get(gateway_name).await?;

    let status = gateway
        .status
        .as_ref()
        .ok_or("Gateway has no status")?;

    let listeners = status
        .listeners
        .as_ref()
        .ok_or("Gateway status has no listeners")?;

    let listener = listeners
        .iter()
        .find(|l| l.name.as_ref() == Some(&listener_name.to_string()))
        .ok_or(format!("Listener {} not found", listener_name))?;

    let listener_json = serde_json::to_value(listener)?;
    let conditions = listener_json["conditions"]
        .as_array()
        .ok_or("Listener has no conditions")?;

    let resolved_refs = conditions
        .iter()
        .find(|c| c["type"] == "ResolvedRefs")
        .ok_or("ResolvedRefs condition not found")?;

    let message = resolved_refs["message"]
        .as_str()
        .ok_or("ResolvedRefs has no message")?;

    if !message.contains(expected_substring) {
        return Err(format!(
            "Expected message to contain '{}', got: '{}'",
            expected_substring, message
        )
        .into());
    }

    println!(
        "✅ Gateway {}/{} listener '{}': message contains '{}'",
        namespace, gateway_name, listener_name, expected_substring
    );

    Ok(())
}

/// Wait for Gateway to be Accepted
pub async fn wait_for_gateway_accepted(
    client: &kube::Client,
    namespace: &str,
    gateway_name: &str,
    timeout_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = std::time::Instant::now();

    while start.elapsed().as_secs() < timeout_secs {
        let gateways: Api<Gateway> = Api::namespaced(client.clone(), namespace);
        let gateway = gateways.get(gateway_name).await?;

        if let Some(status) = &gateway.status {
            if let Some(conditions) = &status.conditions {
                let condition_json = serde_json::to_value(conditions)?;
                if let Some(accepted) = condition_json
                    .as_array()
                    .and_then(|arr| arr.iter().find(|c| c["type"] == "Accepted"))
                {
                    if accepted["status"] == "True" {
                        println!("✅ Gateway {}/{} is Accepted", namespace, gateway_name);
                        return Ok(());
                    }
                }
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }

    Err(format!(
        "Timeout waiting for Gateway {}/{} to be Accepted",
        namespace, gateway_name
    )
    .into())
}
