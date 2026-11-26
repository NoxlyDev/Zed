//! Framework error types.

/// All errors that can be produced by the framework layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrameworkError {
    /// A Discord HTTP call failed.
    #[error("HTTP error: {0}")]
    Http(String),

    /// The command was invoked while the user is on cooldown.
    #[error("Command on cooldown: {remaining_secs:.1}s remaining")]
    Cooldown {
        /// Seconds remaining until the cooldown expires.
        remaining_secs: f64,
    },

    /// The command requires guild context but was run in a DM.
    #[error("This command can only be used in a server")]
    GuildOnly,

    /// The command is restricted to bot owners.
    #[error("This command is restricted to bot owners")]
    OwnerOnly,

    /// The user lacks required permissions.
    #[error("Missing required permissions: {0}")]
    MissingPermissions(String),

    /// The invoking user lacks permissions [`crate::Command::required_permissions`] requires.
    #[error("You are missing permissions to run this command: {missing:?}")]
    MissingUserPermissions {
        /// The permissions the user was missing.
        missing: oxcord_model::Permissions,
    },

    /// The bot itself lacks permissions [`crate::Command::required_bot_permissions`] requires.
    #[error("The bot is missing permissions to run this command: {missing:?}")]
    MissingBotPermissions {
        /// The permissions the bot was missing.
        missing: oxcord_model::Permissions,
    },

    /// [`crate::Command::required_permissions`]/[`crate::Command::required_bot_permissions`] is
    /// set, but the invocation didn't carry a resolved permission value to check it against.
    /// For slash commands this only happens in a DM (Discord doesn't resolve member
    /// permissions there). For prefix commands, the caller is expected to compute
    /// `member_permissions`/`app_permissions` from the gateway cache before calling
    /// [`crate::Framework::dispatch_message`] (the `oxcord` facade's framework bridge does this
    /// automatically when the `cache` feature is enabled); this fires when that wasn't done, or
    /// the cache didn't have the guild/channel/member yet. Fails closed rather than silently
    /// skipping the check.
    #[error("Could not verify permissions for this invocation")]
    PermissionCheckUnavailable,

    /// The autocomplete handler for a named option failed.
    #[error("Autocomplete error for option '{option}': {message}")]
    Autocomplete {
        /// The option name that triggered the error.
        option: String,
        /// The underlying error message.
        message: String,
    },

    /// An unknown or unregistered command was invoked.
    #[error("Unknown command: {0}")]
    UnknownCommand(String),

    /// A user-supplied command handler returned an error.
    #[error("Command error: {0}")]
    Command(Box<dyn std::error::Error + Send + Sync>),

    /// Any other error not covered by the variants above.
    #[error("{0}")]
    Other(String),
}

impl FrameworkError {
    /// Wrap any `Error + Send + Sync` into [`FrameworkError::Command`].
    pub fn command<E: std::error::Error + Send + Sync + 'static>(e: E) -> Self {
        Self::Command(Box::new(e))
    }
}
