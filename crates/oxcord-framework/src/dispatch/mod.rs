//! Command dispatch logic.
//!
//! [`Dispatcher`] receives raw invocation data and routes it to the
//! matching [`Command`] handler, running pre-flight checks (cooldowns,
//! guild-only, owner-only, permissions) before calling the user function.

use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use oxcord_model::application::CommandDataOptionValue;
use oxcord_model::id::UserId;
use tracing::{debug, warn};

use crate::command::Command;
use crate::context::{Context, FrameworkHttp, InvocationData};
use crate::cooldown::CooldownManager;
use crate::error::FrameworkError;
use crate::framework::FrameworkOptions;

/// Internal invocation request built by [`Framework`][crate::Framework] before
/// handing off to the dispatcher.
pub struct Invocation<'a> {
    /// The command name exactly as invoked (without prefix).
    pub command_name: &'a str,
    /// The user who invoked the command.
    pub author: &'a oxcord_model::user::User,
    /// Raw invocation data.
    pub data: &'a InvocationData,
    /// A lightweight HTTP handle.
    pub http: Arc<FrameworkHttp>,
    /// When the invocation was received, before any dispatch work started.
    pub received_at: Instant,
}

/// Routes invocations to registered commands.
pub struct Dispatcher {
    commands: Vec<Command>,
    owners: HashSet<UserId>,
    cooldowns: dashmap::DashMap<String, CooldownManager>,
    user_data: Option<Arc<dyn Any + Send + Sync>>,
}

impl Dispatcher {
    /// Build a new dispatcher from framework options.
    ///
    /// Any command built with [`crate::command::CommandBuilder::cooldown`]
    /// (including via the `#[oxcord_macros::command]` macro) has its cooldown
    /// manager registered automatically — no manual
    /// [`Self::register_cooldown`] call needed.
    #[must_use]
    pub fn new(options: &FrameworkOptions) -> Self {
        let cooldowns = dashmap::DashMap::new();
        for cmd in &options.commands {
            if let Some(config) = cmd.cooldown.clone() {
                cooldowns.insert(cmd.name.clone(), CooldownManager::new(config));
            }
        }
        Self {
            commands: options.commands.clone(),
            owners: options.owners.clone(),
            cooldowns,
            user_data: options.user_data.clone(),
        }
    }

    /// Dispatch an invocation, running all pre-flight checks.
    ///
    /// # Errors
    ///
    /// Returns the first [`FrameworkError`] encountered (cooldown, permission
    /// check, or a propagated handler error).
    pub async fn dispatch(&self, inv: Invocation<'_>) -> Result<(), FrameworkError> {
        let top = self
            .find_command(inv.command_name)
            .ok_or_else(|| FrameworkError::UnknownCommand(inv.command_name.to_owned()))?;

        // --- Resolve subcommands, if any ---
        //
        // A command's own options are only meaningful for its leaf command, so
        // this descends `cmd.subcommands` as far as the invocation picks a
        // branch (slash: Discord's `SubCommand`/`SubCommandGroup` option
        // nesting; prefix: whitespace-separated tokens), rewriting a local copy
        // of the invocation data to carry only the leaf's own options/args.
        // Falls back to the group command itself (which normally has no
        // handler, producing a clear "no handler" error below) if no branch
        // matches.
        let mut resolved_data = inv.data.clone();
        let cmd = Self::resolve_subcommand(top, &mut resolved_data);
        let inv_data = &resolved_data;

        // --- Pre-flight: guild-only ---
        let guild_id = match inv_data {
            InvocationData::Slash {
                guild_id, ..
            }
            | InvocationData::Prefix {
                guild_id, ..
            } => *guild_id,
        };
        if cmd.guild_only == Some(true) && guild_id.is_none() {
            return Err(FrameworkError::GuildOnly);
        }

        // --- Pre-flight: owner-only ---
        if cmd.owner_only && !self.owners.contains(&inv.author.id) {
            return Err(FrameworkError::OwnerOnly);
        }

        // --- Pre-flight: permissions ---
        //
        // Both invocation kinds carry already-resolved `member_permissions`/
        // `app_permissions` by the time they reach here: Discord sends them
        // directly on slash interactions, while for prefix messages the
        // caller (e.g. the `oxcord` facade's framework bridge) computes them
        // from the gateway cache before calling `Framework::dispatch_message`
        // (see its docs). Either way, a `None` here means no resolved value
        // was available (DM, uncached guild/member, or no cache feature) —
        // fail closed rather than silently running an unchecked command.
        if cmd.required_permissions.is_some() || cmd.required_bot_permissions.is_some() {
            let (member_permissions, app_permissions) = match inv_data {
                InvocationData::Slash {
                    member_permissions,
                    app_permissions,
                    ..
                }
                | InvocationData::Prefix {
                    member_permissions,
                    app_permissions,
                    ..
                } => (*member_permissions, *app_permissions),
            };
            if let Some(required) = cmd.required_permissions {
                let Some(have) = member_permissions else {
                    return Err(FrameworkError::PermissionCheckUnavailable);
                };
                let missing = required - have;
                if !missing.is_empty() {
                    return Err(FrameworkError::MissingUserPermissions {
                        missing,
                    });
                }
            }
            if let Some(required) = cmd.required_bot_permissions {
                let Some(have) = app_permissions else {
                    return Err(FrameworkError::PermissionCheckUnavailable);
                };
                let missing = required - have;
                if !missing.is_empty() {
                    return Err(FrameworkError::MissingBotPermissions {
                        missing,
                    });
                }
            }
        }

        // --- Pre-flight: cooldown ---
        let channel_id = inv.http.channel_id;
        if let Some(mgr) = self.cooldowns.get(&cmd.name) {
            if let Some(remaining) = mgr.check(inv.author.id, guild_id, channel_id) {
                return Err(FrameworkError::Cooldown {
                    remaining_secs: remaining.as_secs_f64(),
                });
            }
        }

        // Record invocation before running (prevents double-trigger spam).
        if let Some(mgr) = self.cooldowns.get(&cmd.name) {
            mgr.record(inv.author.id, guild_id, channel_id);
        }

        // --- Dispatch ---
        let Some(handler) = &cmd.handler else {
            warn!(command = %cmd.name, "command has no handler registered");
            return Err(FrameworkError::Other(format!("'{}' has no handler", cmd.name)));
        };

        let ctx = Context {
            author: inv.author,
            channel_id,
            guild_id,
            invocation: inv_data,
            http: inv.http,
            data: self.user_data.clone(),
            received_at: inv.received_at,
        };

        debug!(command = %cmd.name, "dispatching command");
        handler.call(ctx).await
    }

    /// Descend `cmd.subcommands` as far as the invocation selects a branch,
    /// rewriting `data`'s options/args in place to the resolved leaf's own.
    /// See [`Self::dispatch`] for why this exists.
    fn resolve_subcommand<'c>(mut cmd: &'c Command, data: &mut InvocationData) -> &'c Command {
        loop {
            if cmd.subcommands.is_empty() {
                return cmd;
            }
            match data {
                InvocationData::Slash {
                    options, ..
                } => {
                    let Some(opt) = options.first() else {
                        return cmd;
                    };
                    let inner = match &opt.value {
                        CommandDataOptionValue::SubCommand(inner)
                        | CommandDataOptionValue::SubCommandGroup(inner) => inner.clone(),
                        _ => return cmd,
                    };
                    let Some(sub) = cmd.subcommands.iter().find(|s| s.name == opt.name) else {
                        return cmd;
                    };
                    *options = inner;
                    cmd = sub;
                },
                InvocationData::Prefix {
                    args, ..
                } => {
                    let mut parts = args.trim_start().splitn(2, char::is_whitespace);
                    let Some(candidate) = parts.next().filter(|s| !s.is_empty()) else {
                        return cmd;
                    };
                    let Some(sub) =
                        cmd.subcommands.iter().find(|s| s.name == candidate || s.aliases.iter().any(|a| a == candidate))
                    else {
                        return cmd;
                    };
                    *args = parts.next().unwrap_or("").trim_start().to_owned();
                    cmd = sub;
                },
            }
        }
    }

    /// Run the autocomplete handler registered for a named option of a
    /// command, if any, returning its string suggestions.
    ///
    /// # Errors
    ///
    /// Returns [`FrameworkError::UnknownCommand`] if no such command is
    /// registered, or [`FrameworkError::Other`] if that command has no
    /// autocomplete handler for `option_name`.
    pub async fn dispatch_autocomplete<'a>(
        &self,
        command_name: &str,
        option_name: &str,
        partial: String,
        author: &'a oxcord_model::user::User,
        data: &'a InvocationData,
        http: Arc<FrameworkHttp>,
    ) -> Result<Vec<String>, FrameworkError> {
        let top = self
            .find_command(command_name)
            .ok_or_else(|| FrameworkError::UnknownCommand(command_name.to_owned()))?;
        let mut resolved_data = data.clone();
        let cmd = Self::resolve_subcommand(top, &mut resolved_data);
        let data = &resolved_data;
        let Some(handler) = cmd.autocomplete.get(option_name) else {
            return Err(FrameworkError::Other(format!(
                "'{command_name}' has no autocomplete handler for '{option_name}'"
            )));
        };
        let guild_id = match data {
            InvocationData::Slash {
                guild_id, ..
            }
            | InvocationData::Prefix {
                guild_id, ..
            } => *guild_id,
        };
        let ctx = Context {
            author,
            channel_id: http.channel_id,
            guild_id,
            invocation: data,
            http,
            data: self.user_data.clone(),
            received_at: Instant::now(),
        };
        handler.call(ctx, partial).await
    }

    /// Register a cooldown manager for a command by name.
    pub fn register_cooldown(&self, command_name: impl Into<String>, mgr: CooldownManager) {
        self.cooldowns.insert(command_name.into(), mgr);
    }

    fn find_command(&self, name: &str) -> Option<&Command> {
        self.commands.iter().find(|c| c.name == name || c.aliases.iter().any(|a| a == name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandKind;

    fn leaf(name: &str) -> Command {
        Command::builder(name).kind(CommandKind::Both).build()
    }

    fn group(name: &str, subcommands: Vec<Command>) -> Command {
        let mut builder = Command::builder(name).kind(CommandKind::Both);
        for sub in subcommands {
            builder = builder.subcommand(sub);
        }
        builder.build()
    }

    #[test]
    fn prefix_resolves_one_level_of_subcommand_and_strips_the_token() {
        let cmd = group("config", vec![leaf("set"), leaf("get")]);
        let mut data = InvocationData::Prefix {
            args: "set foo bar".to_owned(),
            guild_id: None,
            member_permissions: None,
            app_permissions: None,
        };
        let resolved = Dispatcher::resolve_subcommand(&cmd, &mut data);
        assert_eq!(resolved.name, "set");
        match data {
            InvocationData::Prefix {
                args, ..
            } => assert_eq!(args, "foo bar"),
            InvocationData::Slash {
                ..
            } => panic!("expected Prefix"),
        }
    }

    #[test]
    fn prefix_falls_back_to_group_when_no_branch_matches() {
        let cmd = group("config", vec![leaf("set")]);
        let mut data = InvocationData::Prefix {
            args: "nonexistent".to_owned(),
            guild_id: None,
            member_permissions: None,
            app_permissions: None,
        };
        let resolved = Dispatcher::resolve_subcommand(&cmd, &mut data);
        assert_eq!(resolved.name, "config");
    }

    #[test]
    fn prefix_resolves_through_a_subcommand_group_two_levels_deep() {
        let cmd = group("config", vec![group("prefix", vec![leaf("set"), leaf("get")])]);
        let mut data = InvocationData::Prefix {
            args: "prefix set !".to_owned(),
            guild_id: None,
            member_permissions: None,
            app_permissions: None,
        };
        let resolved = Dispatcher::resolve_subcommand(&cmd, &mut data);
        assert_eq!(resolved.name, "set");
        match data {
            InvocationData::Prefix {
                args, ..
            } => assert_eq!(args, "!"),
            InvocationData::Slash {
                ..
            } => panic!("expected Prefix"),
        }
    }

    #[test]
    fn leaf_command_with_no_subcommands_is_returned_unchanged() {
        let cmd = leaf("ping");
        let mut data = InvocationData::Prefix {
            args: String::new(),
            guild_id: None,
            member_permissions: None,
            app_permissions: None,
        };
        let resolved = Dispatcher::resolve_subcommand(&cmd, &mut data);
        assert_eq!(resolved.name, "ping");
    }
}
