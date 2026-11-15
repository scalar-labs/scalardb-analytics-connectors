//! The state a Connector keeps in each of the SDK's units.

use std::time::Duration;

use crate::{ConnectionOptions, ConnectorError, LibrarySettings, RewriteRuleSet};

use super::{Access, AccessSession, Catalog, Connector, Planner};

/// A Connector's process-level state in an initialized library.
pub(crate) struct ConnectorState<C: Connector> {
    access: <C::Access as Access<C>>::Library,
}

impl<C: Connector> ConnectorState<C> {
    pub(crate) fn new(settings: &LibrarySettings) -> Result<Self, ConnectorError> {
        Ok(Self {
            access: C::Access::open_library(settings)?,
        })
    }

    pub(crate) fn open_data_source(
        &self,
        connector: &str,
        options: ConnectionOptions,
        connection_timeout: Option<Duration>,
    ) -> Result<DataSource<C>, ConnectorError> {
        let settings = C::settings(&options)?;
        Ok(DataSource {
            access: C::Access::open_data_source(
                &self.access,
                connector,
                options,
                connection_timeout,
            )?,
            settings,
        })
    }
}

/// One opened Data Source of Connector `C`.
pub struct DataSource<C: Connector> {
    access: <C::Access as Access<C>>::DataSource,
    settings: C::Settings,
}

impl<C: Connector> DataSource<C> {
    /// The access's state of this Data Source.
    pub fn access(&self) -> &<C::Access as Access<C>>::DataSource {
        &self.access
    }

    /// Opens one session: the access's session, the catalog's context read
    /// on it, the Data Source's settings, and the planner's rules.
    pub(crate) fn open_session(&self) -> Result<Session<C>, ConnectorError> {
        Session::open(
            C::Access::open_session(&self.access)?,
            self.settings.clone(),
        )
    }
}

/// One session of Connector `C`: the unit that runs one request at a time.
pub struct Session<C: Connector> {
    access: AccessSession<C>,
    context: <C::Catalog as Catalog<C>>::Context,
    /// The settings of the Data Source this session belongs to, with which
    /// its table metadata is derived.
    pub(crate) settings: C::Settings,
    rules: RewriteRuleSet<<C::Planner as Planner<C>>::Facts>,
}

impl<C: Connector> Session<C> {
    /// Opens a session on the access's newly opened session state of a Data
    /// Source with `settings`: the catalog reads its context on it, and the
    /// planner's rules are built.
    pub fn open(access: AccessSession<C>, settings: C::Settings) -> Result<Self, ConnectorError> {
        let context = C::Catalog::open_context(&access)?;
        Ok(Self {
            access,
            context,
            settings,
            rules: C::Planner::rewrite_rules(),
        })
    }

    /// The access's state of this session, such as its connection.
    pub fn access(&self) -> &AccessSession<C> {
        &self.access
    }

    /// The facts the catalog read when this session opened.
    pub fn context(&self) -> &<C::Catalog as Catalog<C>>::Context {
        &self.context
    }

    pub(crate) fn rules(&self) -> &RewriteRuleSet<<C::Planner as Planner<C>>::Facts> {
        &self.rules
    }
}
