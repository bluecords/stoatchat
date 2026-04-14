use std::time::Duration;

use crate::{AbstractMFATickets, MFATicket, ReferenceDb};
use iso8601_timestamp::Timestamp;
use revolt_result::Result;
use ulid::Ulid;

#[async_trait]
impl AbstractMFATickets for ReferenceDb {
    /// Find ticket by token
    async fn fetch_ticket_by_token(&self, token: &str) -> Result<MFATicket> {
        let tickets = self.tickets.lock().await;
        let ticket = tickets
            .values()
            .find(|ticket| ticket.token == token)
            .ok_or_else(|| create_error!(InvalidToken))?;

        if let Ok(ulid) = Ulid::from_string(&ticket.id) {
            if Timestamp::from(ulid.datetime() + Duration::from_mins(5)) > Timestamp::now_utc() {
                Ok(ticket.clone())
            } else {
                Err(create_error!(InvalidToken))
            }
        } else {
            Err(create_error!(InvalidToken))
        }
    }

    /// Save ticket
    async fn save_ticket(&self, ticket: &MFATicket) -> Result<()> {
        let mut tickets = self.tickets.lock().await;
        tickets.insert(ticket.id.to_string(), ticket.clone());
        Ok(())
    }

    /// Delete ticket
    async fn delete_ticket(&self, id: &str) -> Result<()> {
        let mut tickets = self.tickets.lock().await;
        if tickets.remove(id).is_some() {
            Ok(())
        } else {
            Err(create_error!(InvalidToken))
        }
    }
}
