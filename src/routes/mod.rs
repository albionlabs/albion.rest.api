pub mod admin;
pub mod health;
pub mod order;
pub mod orders;
pub mod registry;
pub mod swap;
pub mod sync_status;
pub mod tokens;
pub mod trades;
pub mod vaults;

use crate::error::ApiError;
use rain_orderbook_common::raindex_client::vaults::{RaindexVault, RaindexVaultType};

pub(crate) fn resolve_io_vaults(
    order: &rain_orderbook_common::raindex_client::orders::RaindexOrder,
) -> Result<(RaindexVault, RaindexVault), ApiError> {
    let vaults = order.vaults_list().items();
    let (mut input, mut output) = (None, None);
    for v in &vaults {
        match v.vault_type() {
            Some(RaindexVaultType::Input) if input.is_none() => input = Some(v.clone()),
            Some(RaindexVaultType::Output) if output.is_none() => output = Some(v.clone()),
            Some(RaindexVaultType::InputOutput) => {
                if input.is_none() {
                    input = Some(v.clone());
                }
                if output.is_none() {
                    output = Some(v.clone());
                }
            }
            _ => {}
        }
        if input.is_some() && output.is_some() {
            break;
        }
    }
    let input = input.ok_or_else(|| {
        tracing::error!("order has no input vaults");
        ApiError::Internal("order has no input vaults".into())
    })?;
    let output = output.ok_or_else(|| {
        tracing::error!("order has no output vaults");
        ApiError::Internal("order has no output vaults".into())
    })?;
    Ok((input, output))
}

#[cfg(test)]
mod tests;
