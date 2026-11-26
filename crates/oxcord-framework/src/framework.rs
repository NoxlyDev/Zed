//! Top-level [`Framework`] builder and options.

use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use oxcord_builders::{CreateCommand, CreateCommandOption};
use oxcord_model::application::{CommandDataOptionValue, CommandInteraction};
use oxcord_model::channel::ChannelType;
use oxcord_model::id::UserId;
use oxcord_model::user::User;
use oxcord_model::Permissions;
use oxcord_rest::Http;

/// Whether a channel type counts as a thread, for the
/// `SEND_MESSAGES`/`SEND_MESSAGES_IN_THREADS` permission swap (mirrors
/// poise's identical check).
fn is_thread_channel(kind: ChannelType) -> bool {
    matches!(kind, ChannelType::NewsThread | ChannelType::PrivateThread | ChannelType::PublicThread)
}

/// Resolved permissions Discord sends directly on a slash-command
/// interaction, with the thread `SEND_MESSAGES` swap applied.
fn adjust_for_thread(perms: Option<Permissions>, is_thread: bool) -> Option<Permissions> {
    perms.map(|mut p| {
        if is_thread {
            p.set(Permissions::SEND_MESSAGES, p.send_messages_in_threads());
        }
        p
    })
}

/// Find the option Discord marked as focused for autocomplete, descending
/// into `SubCommand`/`SubCommandGroup` option nesting (the focused option is
/// always on the innermost, actually-invoked subcommand, not the top level).
fn find_focused_option(
    options: &[oxcord_model::application::CommandDataOption],
) -> Option<&oxcord_model::application::CommandDataOption> {
    for opt in options {
        match &opt.value {
            CommandDataOptionValue::Autocomplete {
                ..
            } => return Some(opt),
            CommandDataOptionValue::SubCommand(inner) | CommandDataOptionValue::SubCommandGroup(inner) => {
                if let Some(found) = find_focused_option(inner) {
                    return Some(found);
                }
            },
            _ => {},
        }
    }
    None
}

/// A leaf command's own parameters as slash-command options (no subcommands
/// of its own).
fn command_options_as_slash(cmd: &Command) -> Vec<CreateCommandOption> {
    cmd.options
        .iter()
        .map(|opt| {
            CreateCommandOption::new(opt.kind, &opt.name, &opt.description)
                .required(opt.required)
                .set_autocomplete(cmd.autocomplete.contains_key(&opt.name))
        })
        .collect()
}

/// Serialize one subcommand (or subcommand group, if it has subcommands of
/// its own) as a `CreateCommandOption`, recursively. Mirrors poise's
/// `Command::create_as_subcommand`. Discord allows at most two levels of
/// nesting (group -> subcommand); deeper trees than that will be rejected by
/// Discord at registration time, same as poise.
fn command_as_subcommand_option(cmd: &Command) -> Option<CreateCommandOption> {
    if cmd.handler.is_none() && cmd.subcommands.is_empty() {
        // Nothing to invoke and nothing nested under it — skip rather than
        // register a dead-end option.
        return None;
    }

    let kind =
        if cmd.subcommands.is_empty() { oxcord_model::application::CommandOptionType::SubCommand } else { oxcord_model::application::CommandOptionType::SubCommandGroup };
    let mut builder = CreateCommandOption::new(kind, &cmd.name, &cmd.description);

    if cmd.subcommands.is_empty() {
        for option in command_options_as_slash(cmd) {
            builder = builder.add_sub_option(option);
        }
    } else {
        for sub in &cmd.subcommands {
            if let Some(sub_option) = command_as_subcommand_option(sub) {
                builder = builder.add_sub_option(sub_option);
            }
        }
    }

    Some(builder)
}

use crate::command::{Command, CommandKind};
use crate::context::{FrameworkHttp, InvocationData};
use crate::dispatch::{Dispatcher, Invocation};
use crate::error::FrameworkError;

/// Global framework configuration.
#[derive(Default)]
pub struct FrameworkOptions {
    /// All registered commands.
    pub commands: Vec<Command>,
    /// Prefix used for text commands (e.g. `"!"`, `"?"`, `"."`).
    /// `None` disables prefix command support entirely.
    pub prefix: Option<String>,
    /// User IDs considered bot owners (bypass `owner_only` checks).
    pub owners: HashSet<UserId>,
    /// Whether to automatically register slash commands on ready.
    pub auto_register_slash: bool,
    /// Called when a command returns an error.
    #[allow(clippy::type_complexity)]
    pub on_error: Option<Box<dyn Fn(crate::FrameworkError) + Send + Sync>>,
    /// Shared, typed application state every command handler can read back
    /// via `Context::data::<T>()` — the equivalent of poise's generic `Data`
    /// type parameter. Set it with [`FrameworkBuilder::user_data`].
    pub user_data: Option<Arc<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for FrameworkOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameworkOptions")
            .field("commands", &self.commands.iter().map(|c| &c.name).collect::<Vec<_>>())
            .field("prefix", &self.prefix)
            .field("owners", &self.owners)
            .field("auto_register_slash", &self.auto_register_slash)
            .field("user_data", &self.user_data.is_some())
            .finish_non_exhaustive()
    }
}

/// The main framework handle.
///
/// Build it with [`Framework::builder()`] and pass it to your Oxcord client.
///
/// ```rust,no_run,ignore
/// use oxcord_framework::{Framework, FrameworkOptions};
///
/// let framework = Framework::builder()
///     .options(FrameworkOptions {
///         prefix: Some("!".into()),
///         auto_register_slash: true,
///         ..Default::default()
///     })
///     .build();
/// ```
pub struct Framework {
    pub(crate) options: FrameworkOptions,
    pub(crate) dispatcher: Dispatcher,
}

impl std::fmt::Debug for Framework {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Framework").field("options", &self.options).finish_non_exhaustive()
    }
}

impl Framework {
    /// Create a new [`FrameworkBuilder`].
    #[must_use]
    pub fn builder() -> FrameworkBuilder {
        FrameworkBuilder::default()
    }

    /// Return a reference to the registered commands.
    #[must_use]
    pub fn commands(&self) -> &[Command] {
        &self.options.commands
    }

    /// Return the configured prefix, if any.
    #[must_use]
    pub fn prefix(&self) -> Option<&str> {
        self.options.prefix.as_deref()
    }

    /// Try to dispatch a gateway message as a prefix command.
    ///
    /// Returns `None` if no prefix is configured, the message does not start
    /// with it, or the text after the prefix names no registered command
    /// (nothing is dispatched in any of those cases — this is the normal case
    /// for the vast majority of messages a bot sees).
    ///
    /// # Errors
    ///
    /// The inner `Result` is `Err` if a command was found and dispatch was
    /// attempted but failed (cooldown, permission check, or the handler
    /// itself returned an error).
    ///
    /// `member_permissions`/`app_permissions` are the invoking member's and
    /// the bot's own resolved permissions in `channel_id`, if the caller was
    /// able to compute them (typically from the gateway cache — Discord
    /// doesn't send resolved permissions on plain messages the way it does
    /// for interactions). Pass `None` when unavailable (no cache, DM,
    /// uncached guild/member); a command with
    /// [`crate::Command::required_permissions`]/`required_bot_permissions`
    /// then fails closed with [`FrameworkError::PermissionCheckUnavailable`]
    /// rather than running unchecked.
    pub async fn dispatch_message(
        &self,
        author: &User,
        channel_id: oxcord_model::id::ChannelId,
        guild_id: Option<oxcord_model::id::GuildId>,
        content: &str,
        http: Arc<Http>,
        member_permissions: Option<Permissions>,
        app_permissions: Option<Permissions>,
    ) -> Option<Result<(), FrameworkError>> {
        let prefix = self.options.prefix.as_deref()?;
        let rest = content.strip_prefix(prefix)?;
        let mut parts = rest.splitn(2, char::is_whitespace);
        let command_name = parts.next().unwrap_or("").trim();
        if command_name.is_empty() {
            return None;
        }
        let args = parts.next().unwrap_or("").trim_start().to_owned();

        let data = InvocationData::Prefix {
            args,
            guild_id,
            member_permissions,
            app_permissions,
        };
        let framework_http = Arc::new(FrameworkHttp::for_prefix(http, channel_id));
        let inv = Invocation {
            command_name,
            author,
            data: &data,
            http: framework_http,
            received_at: Instant::now(),
        };
        Some(self.dispatcher.dispatch(inv).await)
    }

    /// Dispatch a gateway slash-command interaction.
    ///
    /// # Errors
    ///
    /// Returns [`FrameworkError::UnknownCommand`] if no command by that name
    /// is registered, or any other [`FrameworkError`] the dispatch pipeline
    /// produces (cooldown, permission check, handler error).
    pub async fn dispatch_slash(
        &self,
        command: &CommandInteraction,
        http: Arc<Http>,
    ) -> Result<(), FrameworkError> {
        let is_thread = command.channel.as_ref().is_some_and(|c| is_thread_channel(c.kind));
        let data = InvocationData::Slash {
            token: command.token.clone(),
            guild_id: command.guild_id,
            options: command.data.options.clone(),
            member_permissions: adjust_for_thread(command.member.as_ref().and_then(|m| m.permissions), is_thread),
            app_permissions: adjust_for_thread(command.app_permissions, is_thread),
            is_thread,
        };
        let framework_http =
            Arc::new(FrameworkHttp::for_slash(http, command.id, command.application_id, command.channel_id));
        let inv = Invocation {
            command_name: &command.data.name,
            author: &command.user,
            data: &data,
            http: framework_http,
            received_at: Instant::now(),
        };
        self.dispatcher.dispatch(inv).await
    }

    /// Whether [`FrameworkOptions::auto_register_slash`] is enabled.
    #[must_use]
    pub fn auto_register_slash(&self) -> bool {
        self.options.auto_register_slash
    }

    /// Build the Discord bulk-overwrite payload for every registered slash
    /// command (prefix-only commands are skipped). Each command's options
    /// come from the extra parameters its handler function declared.
    #[must_use]
    pub fn build_global_commands(&self) -> Vec<CreateCommand> {
        self.options
            .commands
            .iter()
            .filter(|cmd| !matches!(cmd.kind, CommandKind::Prefix))
            .map(|cmd| {
                let mut builder = CreateCommand::new(&cmd.name).description(&cmd.description);
                if cmd.guild_only == Some(true) {
                    #[allow(deprecated)]
                    {
                        builder = builder.dm_permission(false);
                    }
                }
                let options = if cmd.subcommands.is_empty() {
                    command_options_as_slash(cmd)
                } else {
                    // A command with subcommands registers each of them as a
                    // `SubCommand`/`SubCommandGroup` option instead of its own
                    // parameters — Discord doesn't allow both on the same command.
                    cmd.subcommands.iter().filter_map(command_as_subcommand_option).collect()
                };
                builder.set_options(options)
            })
            .collect()
    }

    /// Register every slash command with Discord as a single bulk overwrite.
    ///
    /// Call this from your `EventHandler::ready` (or let the
    /// `oxcord::framework::v2_bridge` glue do it for you automatically when
    /// [`Self::auto_register_slash`] is set).
    ///
    /// # Errors
    ///
    /// Returns [`FrameworkError::Http`] if the request to Discord fails.
    pub async fn register_global_commands(&self, http: &Http) -> Result<(), FrameworkError> {
        let commands = self.build_global_commands();
        http.create_global_commands(&commands).await.map_err(|e| FrameworkError::Http(e.to_string()))?;
        Ok(())
    }

    /// Dispatch a gateway autocomplete interaction to the matching option's
    /// autocomplete handler, returning its string suggestions.
    ///
    /// # Errors
    ///
    /// Returns [`FrameworkError::Other`] if the interaction carries no
    /// focused option, or if the matching command has no autocomplete
    /// handler registered for it; returns [`FrameworkError::UnknownCommand`]
    /// if no such command is registered.
    pub async fn dispatch_autocomplete(
        &self,
        command: &CommandInteraction,
        http: Arc<Http>,
    ) -> Result<Vec<String>, FrameworkError> {
        let Some(focused) = find_focused_option(&command.data.options) else {
            return Err(FrameworkError::Other("autocomplete interaction has no focused option".to_owned()));
        };
        let partial = match &focused.value {
            CommandDataOptionValue::Autocomplete {
                value, ..
            } => value.clone(),
            _ => unreachable!("filtered above"),
        };

        let is_thread = command.channel.as_ref().is_some_and(|c| is_thread_channel(c.kind));
        let data = InvocationData::Slash {
            token: command.token.clone(),
            guild_id: command.guild_id,
            options: command.data.options.clone(),
            member_permissions: adjust_for_thread(command.member.as_ref().and_then(|m| m.permissions), is_thread),
            app_permissions: adjust_for_thread(command.app_permissions, is_thread),
            is_thread,
        };
        let framework_http =
            Arc::new(FrameworkHttp::for_slash(http, command.id, command.application_id, command.channel_id));
        self.dispatcher
            .dispatch_autocomplete(&command.data.name, &focused.name, partial, &command.user, &data, framework_http)
            .await
    }
}

/// Fluent builder for [`Framework`].
#[derive(Default)]
pub struct FrameworkBuilder {
    options: FrameworkOptions,
}

impl FrameworkBuilder {
    /// Set the full framework options.
    #[must_use]
    pub fn options(mut self, options: FrameworkOptions) -> Self {
        self.options = options;
        self
    }

    /// Register a single command.
    #[must_use]
    pub fn command(mut self, cmd: Command) -> Self {
        self.options.commands.push(cmd);
        self
    }

    /// Set the prefix.
    #[must_use]
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.options.prefix = Some(prefix.into());
        self
    }

    /// Add a bot owner.
    #[must_use]
    pub fn owner(mut self, id: UserId) -> Self {
        self.options.owners.insert(id);
        self
    }

    /// Enable automatic slash command registration on ready.
    #[must_use]
    pub fn auto_register_slash(mut self) -> Self {
        self.options.auto_register_slash = true;
        self
    }

    /// Set the shared, typed application state every command handler can
    /// read back via `Context::data::<T>()` (poise's `ctx.data()` for
    /// Oxcord). `T` is usually a struct of `Arc<Service>` fields.
    #[must_use]
    pub fn user_data<T: Any + Send + Sync + 'static>(mut self, data: T) -> Self {
        self.options.user_data = Some(Arc::new(data));
        self
    }

    /// Finalise and return the [`Framework`].
    #[must_use]
    pub fn build(self) -> Framework {
        let dispatcher = Dispatcher::new(&self.options);
        Framework {
            options: self.options,
            dispatcher,
        }
    }
}
