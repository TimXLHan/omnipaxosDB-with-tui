use crate::database::Database;
use crate::kv::KVCommand;
use crate::{
    network::{Message, Network},
    OmniPaxosKV,
};
use omnipaxos::ballot_leader_election::Ballot;
use omnipaxos::util::LogEntry;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use omnipaxos_ui::OmniPaxosUI;
use tokio::time;

#[derive(Clone, Copy, Eq, Debug, Ord, PartialOrd, PartialEq, Serialize, Deserialize)]
pub struct Round {
    pub round_num: u32,
    pub leader: u64,
}

impl From<Ballot> for Round {
    fn from(ballot: Ballot) -> Self {
        Self {
            round_num: ballot.n,
            leader: ballot.pid,
        }
    }
}

/// Same as in network actor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum APIResponse {
    Decided(u64),
    Get(String, Option<String>),
    NewRound(Option<Round>),
}

pub struct Server {
    pub omni_paxos_ui: OmniPaxosUI,
    pub omni_paxos: OmniPaxosKV,
    pub network: Network,
    pub database: Database,
    pub last_decided_idx: u64,
    pub last_sent_leader: Option<Ballot>,
}

impl Server {
    async fn process_incoming_msgs(&mut self) {
        let messages = self.network.get_received().await;
        for msg in messages {
            match msg {
                Message::APIRequest(kv_cmd) => match kv_cmd {
                    KVCommand::Get(key) => {
                        let value = self.database.handle_command(KVCommand::Get(key.clone()));
                        let msg = Message::APIResponse(APIResponse::Get(key, value));
                        self.network.send(0, msg).await;
                    }
                    cmd => {
                        self.omni_paxos.append(cmd).unwrap();
                    }
                },
                Message::OmniPaxosMsg(msg) => {
                    self.omni_paxos.handle_incoming(msg);
                }
                _ => unimplemented!(),
            }
        }
    }

    async fn send_outgoing_msgs(&mut self) {
        let messages = self.omni_paxos.outgoing_messages();
        for msg in messages {
            let receiver = msg.get_receiver();
            self.network
                .send(receiver, Message::OmniPaxosMsg(msg))
                .await;
        }
    }

    async fn handle_decided_entries(&mut self) {
        let new_decided_idx = self.omni_paxos.get_decided_idx();
        if self.last_decided_idx < new_decided_idx {
            let decided_entries = self
                .omni_paxos
                .read_decided_suffix(self.last_decided_idx)
                .unwrap();
            self.update_database(decided_entries);
            self.last_decided_idx = new_decided_idx;
            /*** reply client ***/
            let msg = Message::APIResponse(APIResponse::Decided(new_decided_idx));
            self.network.send(0, msg).await
        }
    }

    async fn handle_new_leader(&mut self) {
        // Notify the network_actor of new leader
        let new_ballot = self.omni_paxos.get_current_leader_ballot();
        if self.last_sent_leader != new_ballot {
            self.last_sent_leader = new_ballot;
            let msg = Message::APIResponse(APIResponse::NewRound(new_ballot.map(|b| b.into())));
            self.network.send(0, msg).await;
        }
    }

    fn update_database(&self, decided_entries: Vec<LogEntry<KVCommand>>) {
        for entry in decided_entries {
            match entry {
                LogEntry::Decided(cmd) => {
                    self.database.handle_command(cmd);
                }
                _ => {}
            }
        }
    }

    pub(crate) async fn run(&mut self) {
        let mut msg_interval = time::interval(Duration::from_millis(1));
        let mut tick_interval = time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                biased;
                _ = msg_interval.tick() => {
                    self.process_incoming_msgs().await;
                    self.send_outgoing_msgs().await;
                    self.handle_decided_entries().await;
                    self.handle_new_leader().await;
                },
                _ = tick_interval.tick() => {
                    self.omni_paxos.tick();
                    self.omni_paxos_ui.tick(self.omni_paxos.get_ui_states());
                },
                else => (),
            }
        }
    }
}
