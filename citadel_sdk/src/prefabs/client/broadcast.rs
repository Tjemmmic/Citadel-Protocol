use crate::prefabs::ClientServerRemote;
use crate::prelude::*;
use crate::test_common::wait_for_peers;
use futures::{Future, StreamExt};
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use uuid::Uuid;

/// A kernel that streamlines creating, connecting, and interacting with groups
/// Each group has a single owner, and, each connecting peer must at least be registered
/// to the owner alone. The owner thus serves as an "axis of consent", where each member
/// trusts the owner, and through this trust, transitivity of trust flows to all other
/// future members that connect to the group.
pub struct BroadcastKernel<'a, F, Fut> {
    inner_kernel: Box<dyn NetKernel + 'a>,
    shared: Arc<BroadcastShared>,
    _pd: PhantomData<fn() -> (F, Fut)>,
}

pub struct BroadcastShared {
    route_registers: AtomicBool,
    register_rx: citadel_io::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<PeerSignal>>>,
    register_tx: tokio::sync::mpsc::UnboundedSender<PeerSignal>,
}

/// Before running the [`BroadcastKernel`], each peer must send this request
/// to the protocol. One peer can create a group, allowing others to join the group.
///
/// Each peer may create multiple groups.
///
/// Note: When creating a group, a [`GroupType::Public`] will be created. This means any
/// mutually-registered user to the owner may join the group
pub enum GroupInitRequestType {
    /// Create a new group, under owner, with a list of users that are desired to be invited
    ///
    /// if accept_registrations is true, then, any inbound registrations will automatically
    /// be accepted, simulating a publicly open group to all users on the server.
    Create {
        local_user: UserIdentifier,
        invite_list: Vec<UserIdentifier>,
        group_id: Uuid,
        accept_registrations: bool,
    },
    /// Join a pre-existing group as local_user that is administered by owner, and a group_id
    /// that corresponds to a unique group administered by the particular owner
    ///
    /// Note: ordinarily, local_user must be mutually-register to the owner. However, if do_peer_register
    /// is specified, this will ensure both users are registered before continuing (this is not recommended
    /// for production environments, since the timing of registration between users should be approximately
    /// equal, otherwise, the registration request may expire)
    Join {
        local_user: UserIdentifier,
        owner: UserIdentifier,
        group_id: Uuid,
        do_peer_register: bool,
    },
}

#[async_trait]
impl<'a, F, Fut> PrefabFunctions<'a, GroupInitRequestType> for BroadcastKernel<'a, F, Fut>
where
    F: FnOnce(GroupChannel, ClientServerRemote) -> Fut + Send + 'a,
    Fut: Future<Output = Result<(), NetworkError>> + Send + 'a,
{
    type UserLevelInputFunction = F;
    type SharedBundle = Arc<BroadcastShared>;

    fn get_shared_bundle(&self) -> Self::SharedBundle {
        self.shared.clone()
    }

    #[cfg_attr(
        feature = "localhost-testing",
        tracing::instrument(target = "citadel", skip_all, ret, err(Debug))
    )]
    async fn on_c2s_channel_received(
        connect_success: ConnectionSuccess,
        mut remote: ClientServerRemote,
        arg: GroupInitRequestType,
        fx: Self::UserLevelInputFunction,
        shared: Arc<BroadcastShared>,
    ) -> Result<(), NetworkError> {
        let implicated_cid = connect_success.cid;

        wait_for_peers().await;

        let mut creator_only_accept_inbound_registers = false;

        let request = match arg {
            GroupInitRequestType::Create {
                local_user,
                invite_list,
                group_id,
                accept_registrations,
            } => {
                // ensure local user is registered to each on the invite list
                let mut peers_registered = vec![];

                for peer in &invite_list {
                    let peer = peer
                        .search_peer(implicated_cid, remote.inner.account_manager())
                        .await?
                        .ok_or_else(|| {
                            NetworkError::msg(format!(
                                "[create] User {:?} is not registered to {:?}",
                                peer, &local_user
                            ))
                        })?;
                    peers_registered.push(peer.cid)
                }

                creator_only_accept_inbound_registers = accept_registrations;

                GroupBroadcast::Create(
                    peers_registered,
                    MessageGroupOptions {
                        group_type: GroupType::Public,
                        id: group_id.as_u128(),
                    },
                )
            }

            GroupInitRequestType::Join {
                local_user,
                owner,
                group_id,
                do_peer_register,
            } => {
                // ensure local is registered to owner
                let owner_orig = owner;
                let owner_find = owner_orig
                    .search_peer(implicated_cid, remote.inner.account_manager())
                    .await?;
                let owner = if let Some(owner) = owner_find {
                    Some(owner)
                } else if do_peer_register {
                    let mut handle = remote
                        .inner
                        .propose_target(local_user.clone(), owner_orig.clone())
                        .await?;
                    let _ = handle.register_to_peer().await?;
                    owner_orig
                        .search_peer(implicated_cid, remote.inner.account_manager())
                        .await?
                } else {
                    None
                };

                let owner = owner.ok_or_else(|| {
                    NetworkError::msg(format!(
                        "User {:?} is not registered to {:?}",
                        owner_orig, &local_user
                    ))
                })?;

                GroupBroadcast::RequestJoin(MessageGroupKey {
                    cid: owner.cid,
                    mgid: group_id.as_u128(),
                })
            }
        };

        let request = NodeRequest::GroupBroadcastCommand(GroupBroadcastCommand {
            implicated_cid,
            command: request,
        });

        let mut subscription = remote.inner.send_callback_subscription(request).await?;

        let acceptor_task = if creator_only_accept_inbound_registers {
            shared.route_registers.store(true, Ordering::Relaxed);
            let mut reg_rx = { shared.register_rx.lock().take().unwrap() };
            let mut remote = remote.inner.clone();
            Box::pin(async move {
                // Accept every inbound request, so long as the cid is equal to the
                // cid for this group owner
                while let Some(reg_request) = reg_rx.recv().await {
                    log::trace!(target: "citadel", "owner recv reg_request: {:?}", reg_request);
                    if let PeerSignal::PostRegister(peer_conn, _, _, _, None) = &reg_request {
                        let cid = peer_conn.get_original_target_cid();
                        if cid != implicated_cid {
                            log::warn!(target: "citadel", "Received the wrong CID. Will not accept request");
                            continue;
                        }

                        log::trace!(target: "citadel", "Sending ACCEPT_REQUEST to {}", cid);

                        let _ = responses::peer_register(reg_request, true, &mut remote).await?;
                    }
                }

                Ok::<_, NetworkError>(())
            })
                as Pin<
                    Box<
                        dyn futures::Future<
                                Output = Result<(), citadel_proto::prelude::NetworkError>,
                            > + Send,
                    >,
                >
        } else {
            Box::pin(async move { Ok::<_, NetworkError>(()) })
                as Pin<
                    Box<
                        dyn futures::Future<
                                Output = Result<(), citadel_proto::prelude::NetworkError>,
                            > + Send,
                    >,
                >
        };

        while let Some(event) = subscription.next().await {
            log::trace!(target: "citadel", "{:?} *recv* {:?}", implicated_cid, event);
            match map_errors(event)? {
                NodeResult::GroupChannelCreated(GroupChannelCreated { ticket: _, channel }) => {
                    // in either case, whether owner or not, we get a channel
                    return tokio::try_join!((fx)(channel, remote), acceptor_task).map(|_| ());
                }

                NodeResult::GroupEvent(GroupEvent {
                    implicated_cid: _,
                    ticket: _,
                    event: GroupBroadcast::CreateResponse(None),
                }) => {
                    return Err(NetworkError::InternalError(
                        "Unable to create a message group",
                    ))
                }

                _ => {}
            }
        }

        Ok(())
    }

    fn construct(kernel: Box<dyn NetKernel + 'a>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            shared: Arc::new(BroadcastShared {
                route_registers: AtomicBool::new(false),
                register_rx: citadel_io::Mutex::new(Some(rx)),
                register_tx: tx,
            }),
            inner_kernel: kernel,
            _pd: Default::default(),
        }
    }
}

#[async_trait]
impl<F, Fut> NetKernel for BroadcastKernel<'_, F, Fut> {
    fn load_remote(&mut self, node_remote: NodeRemote) -> Result<(), NetworkError> {
        self.inner_kernel.load_remote(node_remote)
    }

    async fn on_start(&self) -> Result<(), NetworkError> {
        self.inner_kernel.on_start().await
    }

    async fn on_node_event_received(&self, message: NodeResult) -> Result<(), NetworkError> {
        if let NodeResult::PeerEvent(PeerEvent {
            event: ps @ PeerSignal::PostRegister(..),
            ticket: _,
        }) = &message
        {
            if self.shared.route_registers.load(Ordering::Relaxed) {
                return self
                    .shared
                    .register_tx
                    .send(ps.clone())
                    .map_err(|err| NetworkError::Generic(err.to_string()));
            }
        }

        self.inner_kernel.on_node_event_received(message).await
    }

    async fn on_stop(&mut self) -> Result<(), NetworkError> {
        self.inner_kernel.on_stop().await
    }
}

#[cfg(test)]
mod tests {
    use crate::builder::node_builder::NodeBuilder;
    use crate::prefabs::client::broadcast::{BroadcastKernel, GroupInitRequestType};
    use crate::prefabs::client::peer_connection::PeerConnectionKernel;
    use crate::prefabs::client::PrefabFunctions;
    use crate::prelude::*;
    use crate::prelude::{ProtocolRemoteTargetExt, UserIdentifier};
    use crate::test_common::{server_info, wait_for_peers, TestBarrier};
    use citadel_proto::prelude::{GroupBroadcast, NetworkError};
    use futures::prelude::stream::FuturesUnordered;
    use futures::TryStreamExt;
    use rstest::rstest;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use uuid::Uuid;

    #[rstest]
    #[case(3)]
    #[case(4)]
    #[timeout(std::time::Duration::from_secs(90))]
    #[tokio::test(flavor = "multi_thread")]
    async fn group_connect(#[case] peer_count: usize) -> Result<(), Box<dyn std::error::Error>> {
        assert!(peer_count > 1);
        citadel_logging::setup_log();
        TestBarrier::setup(peer_count);

        let client_success = &AtomicUsize::new(0);
        let (server, server_addr) = server_info();

        let client_kernels = FuturesUnordered::new();
        let total_peers = (0..peer_count)
            .map(|_| Uuid::new_v4())
            .collect::<Vec<Uuid>>();
        let group_id = Uuid::new_v4();

        for idx in 0..peer_count {
            let uuid = total_peers.get(idx).cloned().unwrap();

            let request = if idx == 0 {
                // invite list is empty since we will expect the users to post_register to us before attempting to join
                GroupInitRequestType::Create {
                    local_user: UserIdentifier::from(uuid),
                    invite_list: vec![],
                    group_id,
                    accept_registrations: true,
                }
            } else {
                GroupInitRequestType::Join {
                    local_user: UserIdentifier::from(uuid),
                    owner: total_peers.get(0).cloned().unwrap().into(),
                    group_id,
                    do_peer_register: true,
                }
            };

            let client_kernel = BroadcastKernel::new_passwordless_defaults(
                uuid,
                server_addr,
                request,
                move |channel, mut remote| async move {
                    log::trace!(target: "citadel", "***GROUP PEER {}={} CONNECT SUCCESS***", idx,uuid);
                    let _ = client_success.fetch_add(1, Ordering::Relaxed);
                    wait_for_peers().await;
                    let owned_groups = remote.list_owned_groups().await.unwrap();

                    if idx == 0 {
                        assert_eq!(owned_groups.len(), 1);
                    } else {
                        assert_eq!(owned_groups.len(), 0);
                    }

                    wait_for_peers().await;
                    std::mem::drop(channel);
                    remote.shutdown_kernel().await
                },
            ).unwrap();

            let client = NodeBuilder::default().build(client_kernel).unwrap();

            client_kernels.push(async move { client.await.map(|_| ()) });
        }

        let clients = Box::pin(async move { client_kernels.try_collect::<()>().await.map(|_| ()) });

        let res = futures::future::try_select(server, clients).await;
        if let Err(err) = res {
            return match err {
                futures::future::Either::Left(left) => Err(left.0.into_string().into()),

                futures::future::Either::Right(right) => Err(right.0.into_string().into()),
            };
        }

        assert_eq!(client_success.load(Ordering::Relaxed), peer_count);
        Ok(())
    }

    #[rstest]
    #[case(2)]
    #[timeout(std::time::Duration::from_secs(90))]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_manual_group_connect(
        #[case] peer_count: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        /*
           Test a group connection between two registered peers
           who engage in a manual mode
        */
        assert!(peer_count > 1);
        citadel_logging::setup_log();
        TestBarrier::setup(peer_count);

        let client_success = &AtomicBool::new(false);
        let receiver_success = &AtomicBool::new(false);

        let (server, server_addr) = server_info();

        let client_kernels = FuturesUnordered::new();
        let total_peers = (0..peer_count)
            .map(|_| Uuid::new_v4())
            .collect::<Vec<Uuid>>();

        for idx in 0..peer_count {
            let uuid = total_peers.get(idx).cloned().unwrap();
            let peers = total_peers
                .clone()
                .into_iter()
                .filter(|r| r != &uuid)
                .map(UserIdentifier::from)
                .collect::<Vec<UserIdentifier>>();

            let client_kernel = PeerConnectionKernel::new_passwordless_defaults(
                uuid,
                server_addr,
                peers,
                move |mut results, mut remote| async move {
                    let _implicated_cid = remote.conn_type.get_implicated_cid();
                    let mut signals = remote.get_unprocessed_signals_receiver().unwrap();

                    wait_for_peers().await;
                    let conn = results.recv().await.unwrap()?;
                    log::trace!(target: "citadel", "User {} received {:?}", uuid, conn);

                    // one user will create the group, the other will respond
                    if idx == 0 {
                        let _channel = remote
                            .create_group(Some(vec![conn.channel.get_peer_cid().into()]))
                            .await?;
                        log::info!(target: "citadel", "The designated node has finished creating a group");

                        wait_for_peers().await;
                        client_success.store(true, Ordering::Relaxed);
                        return remote.shutdown_kernel().await;
                    } else {
                        // wait until the group host finishes setting up the group
                        while let Some(evt) = signals.recv().await {
                            log::info!(target: "citadel", "Received unprocessed signal: {:?}", evt);
                            match evt {
                                NodeResult::GroupEvent(GroupEvent {
                                    implicated_cid: _,
                                    ticket: _,
                                    event: GroupBroadcast::Invitation(_key),
                                }) => {
                                    let _ = crate::responses::group_invite(
                                        evt,
                                        true,
                                        &mut remote.inner,
                                    )
                                    .await?;
                                }

                                NodeResult::GroupChannelCreated(GroupChannelCreated {
                                    ticket: _,
                                    channel: _chan,
                                }) => {
                                    receiver_success.store(true, Ordering::Relaxed);
                                    log::trace!(target: "citadel", "***PEER {} CONNECT***", uuid);
                                    wait_for_peers().await;
                                    return remote.shutdown_kernel().await;
                                }

                                val => {
                                    log::warn!(target: "citadel", "Unhandled response: {:?}", val)
                                }
                            }
                        }
                    }

                    Err(NetworkError::InternalError(
                        "signals_recv ended unexpectedly",
                    ))
                },
            ).unwrap();

            let client = NodeBuilder::default().build(client_kernel).unwrap();
            client_kernels.push(async move { client.await.map(|_| ()) });
        }

        let clients = Box::pin(async move { client_kernels.try_collect::<()>().await.map(|_| ()) });

        if let Err(err) = futures::future::try_select(server, clients).await {
            return match err {
                futures::future::Either::Left(res) => Err(res.0.into_string().into()),
                futures::future::Either::Right(res) => Err(res.0.into_string().into()),
            };
        }

        assert!(client_success.load(Ordering::Relaxed));
        assert!(receiver_success.load(Ordering::Relaxed));
        Ok(())
    }
}
