use petri_domain::{
    DomainEvent, Marking, PersistedEvent, PlaceId, ReplyRouting, Token, TokenColor, TokenId,
};

use crate::schema_registry::SchemaRegistry;
use crate::{EventRepository, ServiceError, TopologyRepository};

/// Create a new token at a place, optionally attaching reply routing context.
pub(crate) async fn create_token_with_meta<E: EventRepository, T: TopologyRepository>(
    events: &E,
    topology: &T,
    place_id: PlaceId,
    color: TokenColor,
    reply_routing: Option<ReplyRouting>,
    signal_key: Option<String>,
    dedup_id: Option<String>,
    schema_registry: Option<&SchemaRegistry>,
) -> Result<PersistedEvent, ServiceError> {
    let net = topology.get_topology().ok_or(ServiceError::NoTopology)?;

    // Verify place exists
    let place = net
        .get_place(&place_id)
        .ok_or_else(|| ServiceError::PlaceNotFound(place_id.clone()))?;

    // Validate token data against place's token_schema if present
    if let (Some(registry), Some(ref schema_ref)) = (schema_registry, &place.token_schema) {
        if let TokenColor::Data(ref data) = color {
            if let Err(e) = registry.validate(schema_ref, data) {
                return Err(ServiceError::SchemaValidationFailed {
                    port_name: place.name.clone(),
                    transition_id: petri_domain::TransitionId::new(), // no transition context
                    error: format!("Token injection at place '{}': {}", place.name, e),
                });
            }
        }
    }

    let mut token = Token::new(color);
    if let Some(routing) = reply_routing {
        token = token.with_reply_routing(routing);
    }
    let event = events
        .append(DomainEvent::TokenCreated {
            token,
            place_id,
            place_name: None,
            workflow_id: None,
            signal_key,
            dedup_id,
        })
        .await?;

    Ok(event)
}

/// Remove a token from a place by token ID or correlation ID.
pub(crate) async fn remove_token<E: EventRepository, T: TopologyRepository>(
    events: &E,
    topology: &T,
    marking: &Marking,
    place_id: PlaceId,
    token_id: Option<TokenId>,
    correlation_id: Option<String>,
    reason: Option<String>,
) -> Result<PersistedEvent, ServiceError> {
    let net = topology.get_topology().ok_or(ServiceError::NoTopology)?;

    // Verify place exists
    net.get_place(&place_id)
        .ok_or_else(|| ServiceError::PlaceNotFound(place_id.clone()))?;

    let tokens = marking.tokens_at(&place_id);

    // Find the token to remove
    let target_token = if let Some(tid) = &token_id {
        tokens.iter().find(|t| &t.id == tid)
    } else if let Some(cid) = &correlation_id {
        tokens.iter().find(|t| {
            if let TokenColor::Data(data) = &t.color {
                data.get("id")
                    .or_else(|| data.get("correlation_id"))
                    .or_else(|| data.get("job_id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s == cid)
                    .unwrap_or(false)
            } else {
                false
            }
        })
    } else {
        return Err(ServiceError::InvalidOperation(
            "Either token_id or correlation_id must be provided".to_string(),
        ));
    };

    let token = target_token.ok_or_else(|| {
        ServiceError::TokenNotFound(token_id.clone().unwrap_or_default(), place_id.clone())
    })?;

    let event = events
        .append(DomainEvent::TokenRemoved {
            token_id: token.id.clone(),
            place_id,
            reason,
            correlation_id,
        })
        .await?;

    Ok(event)
}

/// Update a token's data in place.
pub(crate) async fn update_token<E: EventRepository, T: TopologyRepository>(
    events: &E,
    topology: &T,
    marking: &Marking,
    place_id: PlaceId,
    token_id: Option<TokenId>,
    correlation_id: Option<String>,
    new_color: TokenColor,
) -> Result<PersistedEvent, ServiceError> {
    let net = topology.get_topology().ok_or(ServiceError::NoTopology)?;

    // Verify place exists
    net.get_place(&place_id)
        .ok_or_else(|| ServiceError::PlaceNotFound(place_id.clone()))?;

    let tokens = marking.tokens_at(&place_id);

    // Find the token to update
    let target_token = if let Some(tid) = &token_id {
        tokens.iter().find(|t| &t.id == tid)
    } else if let Some(cid) = &correlation_id {
        tokens.iter().find(|t| {
            if let TokenColor::Data(data) = &t.color {
                data.get("id")
                    .or_else(|| data.get("correlation_id"))
                    .or_else(|| data.get("job_id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s == cid)
                    .unwrap_or(false)
            } else {
                false
            }
        })
    } else {
        return Err(ServiceError::InvalidOperation(
            "Either token_id or correlation_id must be provided".to_string(),
        ));
    };

    let token = target_token.ok_or_else(|| {
        ServiceError::TokenNotFound(token_id.clone().unwrap_or_default(), place_id.clone())
    })?;

    let event = events
        .append(DomainEvent::TokenUpdated {
            token_id: token.id.clone(),
            place_id,
            new_color,
            correlation_id,
        })
        .await?;

    Ok(event)
}
