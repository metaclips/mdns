use super::*;
use crate::errors::*;

use util::Error;

// pack_bytes appends the wire format of field to msg.
pub(crate) fn pack_bytes(mut msg: Vec<u8>, field: &[u8]) -> Vec<u8> {
    msg.extend_from_slice(field);
    msg
}

pub(crate) fn unpack_bytes(msg: &[u8], off: usize, field: &mut [u8]) -> Result<usize, Error> {
    let new_off = off + field.len();
    if new_off > msg.len() {
        return Err(ERR_BASE_LEN.to_owned());
    }
    field.copy_from_slice(&msg[off..new_off]);
    Ok(new_off)
}

// pack_uint16 appends the wire format of field to msg.
pub(crate) fn pack_uint16(mut msg: Vec<u8>, field: u16) -> Vec<u8> {
    msg.extend_from_slice(&field.to_be_bytes());
    msg
}

pub(crate) fn unpack_uint16(msg: &[u8], off: usize) -> Result<(u16, usize), Error> {
    if off + UINT16LEN > msg.len() {
        return Err(ERR_BASE_LEN.to_owned());
    }

    Ok((
        (msg[off] as u16) << 8 | (msg[off + 1] as u16),
        off + UINT16LEN,
    ))
}

pub(crate) fn skip_uint16(msg: &[u8], off: usize) -> Result<usize, Error> {
    if off + UINT16LEN > msg.len() {
        return Err(ERR_BASE_LEN.to_owned());
    }
    Ok(off + UINT16LEN)
}

// pack_uint32 appends the wire format of field to msg.
pub(crate) fn pack_uint32(mut msg: Vec<u8>, field: u32) -> Vec<u8> {
    msg.extend_from_slice(&field.to_be_bytes());
    msg
}

pub(crate) fn unpack_uint32(msg: &[u8], off: usize) -> Result<(u32, usize), Error> {
    if off + UINT32LEN > msg.len() {
        return Err(ERR_BASE_LEN.to_owned());
    }
    let v = (msg[off] as u32) << 24
        | (msg[off + 1] as u32) << 16
        | (msg[off + 2] as u32) << 8
        | (msg[off + 3] as u32);
    Ok((v, off + UINT32LEN))
}

pub(crate) fn skip_uint32(msg: &[u8], off: usize) -> Result<usize, Error> {
    if off + UINT32LEN > msg.len() {
        return Err(ERR_BASE_LEN.to_owned());
    }
    Ok(off + UINT32LEN)
}

// pack_text appends the wire format of field to msg.
pub(crate) fn pack_str(mut msg: Vec<u8>, field: &str) -> Result<Vec<u8>, Error> {
    let l = field.len();
    if l > 255 {
        return Err(ERR_STRING_TOO_LONG.to_owned());
    }
    msg.push(l as u8);
    msg.extend_from_slice(field.as_bytes());
    Ok(msg)
}

pub(crate) fn unpack_str(msg: &[u8], off: usize) -> Result<(String, usize), Error> {
    if off >= msg.len() {
        return Err(ERR_BASE_LEN.to_owned());
    }
    let begin_off = off + 1;
    let end_off = begin_off + msg[off] as usize;
    if end_off > msg.len() {
        return Err(ERR_CALC_LEN.to_owned());
    }

    Ok((
        String::from_utf8(msg[begin_off..end_off].to_vec())?,
        end_off,
    ))
}
