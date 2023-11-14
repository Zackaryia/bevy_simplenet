//local shortcuts

//third-party shortcuts

//standard shortcuts
use core::fmt::Debug;

//-------------------------------------------------------------------------------------------------------------------

/// Errors emitted by the internal client handler.
#[derive(Debug)]
pub enum ClientError
{
    //ClosedByServer,
    SendError
}

impl std::fmt::Display for ClientError
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result
    {
        let _ = write!(f, "ClientError::");
        match self
        {
            //ClientError::ClosedByServer => write!(f, "ClosedByServer"),
            ClientError::SendError      => write!(f, "SendError"),
        }
    }
}
impl std::error::Error for ClientError {}

//-------------------------------------------------------------------------------------------------------------------
