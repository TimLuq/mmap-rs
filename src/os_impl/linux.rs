use crate::areas::{MemoryArea, Protection, ShareMode};
use crate::error::Error;
use combine::{
    EasyParser, Parser, Stream,
    error::ParseError,
    parser::{
        char::hex_digit,
        repeat::many1,
    },
    token,
};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::io::Lines;
use std::ops::Range;
use std::path::PathBuf;

fn hex_digit1<Input>() -> impl Parser<Input, Output = String>
where
    Input: Stream<Token = char>,
{
    many1(hex_digit())
}

fn address_range<Input>() -> impl Parser<Input, Output = Range<usize>>
where
    Input: Stream<Token = char>,
    <Input::Error as ParseError<Input::Token, Input::Range, Input::Position>>::StreamError:
        From<::std::num::ParseIntError>,
{
    (
        hex_digit1().and_then(|s| usize::from_str_radix(s.as_str(), 16)),
        token('-'),
        hex_digit1().and_then(|s| usize::from_str_radix(s.as_str(), 16)),
    )
        .map(|(start, _, end)| start..end)
}

fn permissions<Input>() -> impl Parser<Input, Output = (Protection, ShareMode)>
where
    Input: Stream<Token = char>,
{
    use combine::parser::{
        char::char,
        choice::or,
    };

    (
        or(char('r').map(|_| Protection::READ), char('-').map(|_| Protection::empty())),
        or(char('w').map(|_| Protection::WRITE), char('-').map(|_| Protection::empty())),
        or(char('x').map(|_| Protection::EXECUTE), char('-').map(|_| Protection::empty())),
        or(char('s').map(|_| ShareMode::Shared), char('p').map(|_| ShareMode::Private)),
    )
        .map(|(r, w, x, s)| (r | w | x, s))
}

fn device_id<Input>() -> impl Parser<Input, Output = (u8, u8)>
where
    Input: Stream<Token = char>,
    <Input::Error as ParseError<Input::Token, Input::Range, Input::Position>>::StreamError:
        From<::std::num::ParseIntError>,
{
    (
        hex_digit1().and_then(|s| u8::from_str_radix(s.as_str(), 16)),
        token(':'),
        hex_digit1().and_then(|s| u8::from_str_radix(s.as_str(), 16)),
    )
        .map(|(major, _, minor)| (major, minor))
}

fn path<Input>() -> impl Parser<Input, Output = PathBuf>
where
    Input: Stream<Token = char>,
{
    use combine::parser::token::satisfy;

    many1(satisfy(|c| c != '\n'))
        .map(|s: String| PathBuf::from(s))
}

fn memory_region<Input>() -> impl Parser<Input, Output = MemoryArea>
where
    Input: Stream<Token = char>,
    <Input::Error as ParseError<Input::Token, Input::Range, Input::Position>>::StreamError:
        From<::std::num::ParseIntError>,
{
    use combine::parser::{
        char::spaces,
        choice::optional,
    };

    (
        address_range(),
        spaces(),
        permissions(),
        spaces(),
        hex_digit1().and_then(|s| u64::from_str_radix(s.as_str(), 16)),
        spaces(),
        device_id(),
        spaces(),
        hex_digit1(),
        spaces(),
        optional(path()),
    )
        .map(|(range, _, (protection, share_mode), _, offset, _, _, _, _, _, path)| {
            let share_mode = if path.is_some() && share_mode == ShareMode::Private {
                ShareMode::CopyOnWrite
            } else {
                share_mode
            };

            MemoryArea {
                range,
                protection,
                share_mode,
                path: path.map(|path| (path, offset)),
            }
        })
}

pub struct MemoryAreas<B> {
    lines: Lines<B>,
}

impl MemoryAreas<BufReader<File>> {
    pub fn open(pid: Option<u32>) -> Result<Self, Error> {
        let path = match pid {
            Some(pid) => format!("/proc/{}/maps", pid),
            _ => "/proc/self/maps".to_string(),
        };

        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let lines = reader.lines();

        Ok(Self {
            lines,
        })
    }
}

impl<B: BufRead> Iterator for MemoryAreas<B> {
    type Item = Result<MemoryArea, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let line = match self.lines.next() {
            Some(Ok(line)) => line,
            Some(Err(e)) => return Some(Err(Error::Io(e))),
            None => return None,
        };

        use combine::stream::position::Stream;

        let result = match memory_region().easy_parse(Stream::new(line.as_str())) {
            Ok((region, _)) => Some(Ok(region)),
            _ => None,
        };

        result
    }
}
