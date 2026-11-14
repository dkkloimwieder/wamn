//! Shared execution of typed application exports.

use crate::InputError;

/// Refuse an envelope before a handler acquires SQL or runs an override.
pub fn validate_envelope<'a>(
    count: usize,
    bounds: std::ops::RangeInclusive<usize>,
    mut correlations: impl Iterator<Item = &'a str>,
) -> Result<(), InputError> {
    if !bounds.contains(&count) {
        return Err(InputError::new("invalid request element count"));
    }
    if correlations.any(str::is_empty) {
        return Err(InputError::new("request_id is empty"));
    }
    Ok(())
}

/// Validate one request before calling its application handler.
pub async fn call<R, V, E, S, O>(
    request: Result<R, V>,
    state: &mut S,
    mut normalize: impl FnMut(&mut R) -> Result<(), V>,
    invalid: impl Fn(V) -> E,
    mut handler: impl AsyncFnMut(&mut S, R) -> Result<O, E>,
) -> Result<O, E> {
    let mut request = request.map_err(&invalid)?;
    normalize(&mut request).map_err(invalid)?;
    handler(state, request).await
}

/// Keep correlation while executing each validated request in order.
pub async fn dispatch<K, R, V, E, S, O>(
    input: impl IntoIterator<Item = (K, Result<R, V>)>,
    state: &mut S,
    mut normalize: impl FnMut(&mut R) -> Result<(), V>,
    invalid: impl Fn(V) -> E,
    mut handler: impl AsyncFnMut(&mut S, R) -> Result<O, E>,
) -> Vec<(K, Result<O, E>)> {
    let mut output = Vec::new();
    for (correlation, request) in input {
        let result = call(request, state, &mut normalize, &invalid, &mut handler).await;
        output.push((correlation, result));
    }
    output
}

/// Export an operation whose host owns the transaction.
#[macro_export]
macro_rules! export_transaction {
    ($component:ty, $contract:path, $node:path, $handler:path, $operation:ident) => {
        const _: () = {
            use $contract as __contract;
            use $node as __node;
            use $operation as __operation;
            impl __contract::Guest for $component {
                async fn run(
                    _context: __node::NodeContext,
                    input: __operation::Input,
                ) -> Result<__operation::Output, __node::NodeError> {
                    __operation::validate(&input).map_err(|error| {
                        __node::NodeError::InvalidInput(__node::ErrorDetail {
                            message: error.context().into(),
                            code: Some("invalid_input".into()),
                        })
                    })?;
                    let mut state = (
                        None::<wamn_postgres_statements::Transaction>,
                        None::<__node::NodeError>,
                    );
                    let output = __operation::run(input, &mut state, async |state, request| {
                        if state.1.is_some() {
                            return Err(__operation::OperationError::InternalError);
                        }
                        if state.0.is_none() {
                            match wamn_postgres_statements::operation_transaction().await {
                                Ok(transaction) => state.0 = Some(transaction),
                                Err(error) => {
                                    state.1 =
                                        Some(__node::NodeError::Terminal(__node::ErrorDetail {
                                            message: error.to_string(),
                                            code: Some("internal_error".into()),
                                        }));
                                    return Err(__operation::OperationError::InternalError);
                                }
                            }
                        }
                        $handler(
                            state
                                .0
                                .as_mut()
                                .expect("operation transaction was acquired"),
                            request,
                        )
                        .await
                    })
                    .await;
                    match state.1.take() {
                        Some(error) => Err(error),
                        None => Ok(output),
                    }
                }
            }
        };
    };
}

/// Export a batch operation using application-owned state.
#[macro_export]
macro_rules! export_batch {
    ($component:ty, $contract:path, $node:path, $state:expr, $handler:path, $operation:ident) => {
        const _: () = {
            use $contract as __contract;
            use $node as __node;
            use $operation as __operation;
            impl __contract::Guest for $component {
                async fn run(
                    _context: __node::NodeContext,
                    input: __operation::Input,
                ) -> Result<__operation::Output, __node::NodeError> {
                    __operation::validate(&input).map_err(|error| {
                        __node::NodeError::InvalidInput(__node::ErrorDetail {
                            message: error.context().into(),
                            code: Some("invalid_input".into()),
                        })
                    })?;
                    let mut state = $state;
                    Ok(__operation::run(input, &mut state, $handler).await)
                }
            }
        };
    };
}

/// Export a single typed request with no outcome envelope.
#[macro_export]
macro_rules! export_direct {
    ($component:ty, $contract:path, $node:path, $state:expr, $handler:path, $operation:ident) => {
        const _: () = {
            use $contract as __contract;
            use $node as __node;
            use $operation as __operation;
            impl __contract::Guest for $component {
                async fn run(
                    _context: __node::NodeContext,
                    mut input: __operation::Input,
                ) -> Result<__operation::Output, __node::NodeError> {
                    __operation::normalize(&mut input).map_err(|error| {
                        __node::NodeError::InvalidInput(__node::ErrorDetail {
                            message: error.context().into(),
                            code: Some("invalid_input".into()),
                        })
                    })?;
                    let mut state = $state;
                    $handler(&mut state, input).await
                }
            }
        };
    };
}

/// Export one query. Its read metadata stays with the host authority.
#[macro_export]
macro_rules! export_query {
    ($component:ty, $contract:path, $node:path, $state:expr, $handler:path, $operation:ident) => {
        const _: () = {
            use $contract as __contract;
            use $node as __node;
            use $operation as __operation;
            impl __contract::Guest for $component {
                async fn run(
                    _context: __node::NodeContext,
                    input: __operation::Input,
                ) -> Result<__operation::Output, __node::NodeError> {
                    Ok(__contract::QueryOutcome {
                        outcome: __operation::run(Ok(input), &mut $state, $handler).await,
                    })
                }
            }
        };
    };
}

#[cfg(test)]
mod tests {
    use std::future::Future as _;
    use std::task::{Context, Poll, Waker};

    #[test]
    fn dispatch_preserves_correlation_and_validates_before_the_handler() {
        let mut calls = Vec::new();
        let call = super::dispatch(
            [(1, Ok(2)), (2, Err(())), (3, Ok(-1)), (4, Ok(4))],
            &mut calls,
            |request| if *request >= 0 { Ok(()) } else { Err(()) },
            |()| (),
            async |calls, request| {
                calls.push(request);
                Ok(request * 2)
            },
        );
        let Poll::Ready(outcomes) =
            std::pin::pin!(call).poll(&mut Context::from_waker(Waker::noop()))
        else {
            panic!("a synchronous handler finishes in one poll");
        };
        assert_eq!(
            outcomes,
            [(1, Ok(4)), (2, Err(())), (3, Err(())), (4, Ok(8))]
        );
        assert_eq!(calls, [2, 4]);
    }
}
