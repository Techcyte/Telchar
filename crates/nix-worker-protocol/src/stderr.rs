//! Encodes bounded structured stderr frames and terminal worker errors.

use std::io::{self, Write};

use crate::{
    write_worker_byte_string_to, write_worker_integer_to, ProtocolError,
    MAXIMUM_STRUCTURED_FRAME_FIELDS, MAXIMUM_STRUCTURED_FRAME_FIELD_BYTES,
    MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES, STDERR_ERROR, STDERR_LAST, STDERR_NEXT, STDERR_RESULT,
    STDERR_START_ACTIVITY, STDERR_STOP_ACTIVITY,
};

pub fn write_worker_error(output: &mut impl Write, message: &str) -> io::Result<()> {
    if message.len() > MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "structured frame exceeds limit",
        ));
    }
    write_worker_integer_to(output, STDERR_ERROR)?;
    write_worker_byte_string_to(output, b"Error")?;
    write_worker_integer_to(output, 0)?;
    write_worker_byte_string_to(output, b"Error")?;
    write_worker_byte_string_to(output, message.as_bytes())?;
    write_worker_integer_to(output, 0)?;
    write_worker_integer_to(output, 0)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivityField {
    Integer(u64),
    String(Vec<u8>),
}

/// A structured stderr frame emitted by the worker during a build operation.
///
/// These frames carry progress information, activity markers, and result data
/// back to the client over the Nix worker protocol. Each variant matches the
/// wire format defined by the stock `nix-daemon --stdio` implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StderrFrame {
    Next {
        message: Vec<u8>,
    },
    StartActivity {
        activity_id: u64,
        verbosity: u64,
        activity_type: u64,
        message: Vec<u8>,
        fields: Vec<ActivityField>,
        parent_activity_id: u64,
    },
    StopActivity {
        activity_id: u64,
    },
    Result {
        activity_id: u64,
        result_type: u64,
        fields: Vec<ActivityField>,
    },
    Last,
}

/// Write a structured stderr frame to the output stream.
///
/// This function serializes any `StderrFrame` variant into the Nix worker
/// protocol wire format, matching the behaviour of stock `nix-daemon --stdio`
/// as observed in captured traffic.
pub fn write_stderr_frame(output: &mut impl Write, frame: StderrFrame) -> io::Result<()> {
    validate_stderr_frame(&frame).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "structured frame exceeds limit",
        )
    })?;
    match frame {
        StderrFrame::Next { message } => {
            write_worker_integer_to(output, STDERR_NEXT)?;
            write_worker_byte_string_to(output, &message)?;
        }
        StderrFrame::StartActivity {
            activity_id,
            verbosity,
            activity_type,
            message,
            fields,
            parent_activity_id,
        } => {
            write_worker_integer_to(output, STDERR_START_ACTIVITY)?;
            write_worker_integer_to(output, activity_id)?;
            write_worker_integer_to(output, verbosity)?;
            write_worker_integer_to(output, activity_type)?;
            write_worker_byte_string_to(output, &message)?;
            write_activity_fields(output, &fields)?;
            write_worker_integer_to(output, parent_activity_id)?;
        }
        StderrFrame::StopActivity { activity_id } => {
            write_worker_integer_to(output, STDERR_STOP_ACTIVITY)?;
            write_worker_integer_to(output, activity_id)?;
        }
        StderrFrame::Result {
            activity_id,
            result_type,
            fields,
        } => {
            write_worker_integer_to(output, STDERR_RESULT)?;
            write_worker_integer_to(output, activity_id)?;
            write_worker_integer_to(output, result_type)?;
            write_activity_fields(output, &fields)?;
        }
        StderrFrame::Last => {
            write_worker_integer_to(output, STDERR_LAST)?;
        }
    }
    Ok(())
}

fn write_activity_fields(output: &mut impl Write, fields: &[ActivityField]) -> io::Result<()> {
    write_worker_integer_to(output, fields.len() as u64)?;
    for field in fields {
        match field {
            ActivityField::Integer(value) => {
                write_worker_integer_to(output, 0)?;
                write_worker_integer_to(output, *value)?;
            }
            ActivityField::String(value) => {
                write_worker_integer_to(output, 1)?;
                write_worker_byte_string_to(output, value)?;
            }
        }
    }
    Ok(())
}

fn validate_stderr_frame(frame: &StderrFrame) -> Result<(), ProtocolError> {
    let (message, fields) = match frame {
        StderrFrame::Next { message } => {
            if message.len() > MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES {
                return Err(ProtocolError::SizeLimit);
            }
            return Ok(());
        }
        StderrFrame::StartActivity {
            message, fields, ..
        } => (Some(message), fields),
        StderrFrame::StopActivity { .. } | StderrFrame::Last => return Ok(()),
        StderrFrame::Result { fields, .. } => (None, fields),
    };
    if message.is_some_and(|message| message.len() > MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES)
        || fields.len() > MAXIMUM_STRUCTURED_FRAME_FIELDS
        || fields.iter().any(|field| {
            matches!(field, ActivityField::String(value) if value.len() > MAXIMUM_STRUCTURED_FRAME_FIELD_BYTES)
        })
    {
        return Err(ProtocolError::SizeLimit);
    }
    Ok(())
}
