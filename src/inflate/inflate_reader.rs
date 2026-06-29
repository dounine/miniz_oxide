use binrw::io::Read;
use binrw::io::Seek;
use binrw::io::Write;
use std::io::SeekFrom;
use crate::error::Error;
use crate::inflate::stream::{InflateState, inflate};
use crate::{DataFormat, MZFlush};

///  InflateReader 操作模式
enum ReaderMode {
    /// 检测中
    Detecting,
    /// 解压模式
    Decompressing,
    /// 直通模式（直接读取）
    PassThrough,
}

/// 1. 按需解压数据
/// 2. 已解压的数据保存在内部缓冲区中，支持随机访问已解压数据
/// 3. 自动检测：如果数据是压缩的则解压，否则直接读取
pub struct InflateReader<R> {
    inner: R,
    mode: ReaderMode,
    // 解压模式需要的字段
    decomp_state: Option<Box<InflateState>>,
    // 内部缓冲区（仅解压模式使用）
    buffer: Vec<u8>,
    buffer_pos: usize,
    // 输入缓冲（仅解压模式使用）
    input_buffer: Vec<u8>,
    input_offset: usize,
    input_end: usize,
    is_eof: bool,
    // 检测时的预读数据（仅检测模式后直通模式使用）
    peek_buffer: Vec<u8>,
    peek_pos: usize,
}

impl<R> InflateReader<R> {
    /// 创建新的 InflateReader（自动检测模式）
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            mode: ReaderMode::Detecting,
            decomp_state: Some(InflateState::new_boxed(DataFormat::Raw)),
            buffer: Vec::new(),
            buffer_pos: 0,
            input_buffer: vec![0; 32 * 1024],
            input_offset: 0,
            input_end: 0,
            is_eof: false,
            peek_buffer: Vec::new(),
            peek_pos: 0,
        }
    }

    /// 创建新的 InflateReader，强制使用解压模式
    pub fn new_decompress(inner: R) -> Self {
        Self {
            inner,
            mode: ReaderMode::Decompressing,
            decomp_state: Some(InflateState::new_boxed(DataFormat::Raw)),
            buffer: Vec::new(),
            buffer_pos: 0,
            input_buffer: vec![0; 32 * 1024],
            input_offset: 0,
            input_end: 0,
            is_eof: false,
            peek_buffer: Vec::new(),
            peek_pos: 0,
        }
    }

    /// 创建新的 InflateReader，强制使用直通模式
    pub fn new_passthrough(inner: R) -> Self {
        Self {
            inner,
            mode: ReaderMode::PassThrough,
            decomp_state: None,
            buffer: Vec::new(),
            buffer_pos: 0,
            input_buffer: Vec::new(), // 直通模式不需要预分配
            input_offset: 0,
            input_end: 0,
            is_eof: false,
            peek_buffer: Vec::new(),
            peek_pos: 0,
        }
    }

    /// 消费 InflateReader，返回内部 reader
    pub fn into_inner(self) -> R {
        self.inner
    }

    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// 获取已解压数据的长度（仅解压模式有效）
    pub fn decompressed_len(&self) -> usize {
        self.buffer.len()
    }

    /// 随机访问已解压数据（仅解压模式有效）
    pub fn get(&self, index: usize) -> Option<&u8> {
        self.buffer.get(index)
    }

    /// 获取已解压数据的切片（仅解压模式有效）
    pub fn get_slice(&self, start: usize, end: usize) -> Option<&[u8]> {
        if start <= end && end <= self.buffer.len() {
            Some(&self.buffer[start..end])
        } else {
            None
        }
    }

    /// 获取所有已解压数据（仅解压模式有效）
    pub fn get_all(&self) -> &[u8] {
        &self.buffer
    }

    /// 重置 buffer 位置到开头（仅解压模式有效）
    pub fn reset_position(&mut self) {
        self.buffer_pos = 0;
    }
}

impl<R: Read + Send> InflateReader<R> {
    /// 内部方法：检测数据是否是压缩数据
    async fn try_detect(&mut self) -> Result<bool, Error> {
        // 读取一些输入数据用于检测
        let mut test_input = Vec::new();
        let n = self.inner.read(&mut self.input_buffer).await?;
        if n > 0 {
            test_input.extend_from_slice(&self.input_buffer[0..n]);
            self.peek_buffer.extend_from_slice(&self.input_buffer[0..n]);
        }

        if test_input.is_empty() {
            // 没有数据，默认直通模式
            return Ok(false);
        }

        // 尝试解压来检测
        let mut test_writer = VecWriter { buf: Vec::new() };
        let mut test_decomp = InflateState::new_boxed(DataFormat::Raw);

        let result = inflate(
            &mut test_decomp,
            &test_input,
            &mut test_writer,
            MZFlush::None,
        ).await;

        match result {
            Ok(stream_result) => {
                match stream_result.status {
                    Ok(crate::MZStatus::Ok) | Ok(crate::MZStatus::StreamEnd) => {
                        // 可以解压，使用解压模式
                        return Ok(true);
                    }
                    _ => {}
                }
            }
            Err(_) => {}
        }

        // 不能解压，使用直通模式
        Ok(false)
    }

    /// 内部方法：按需解压更多数据到缓冲区（仅解压模式）
    async fn decompress_more(&mut self) -> Result<bool, Error> {
        if self.is_eof {
            return Ok(false);
        }

        let mut writer = BufferWriter::new(&mut self.buffer);

        loop {
            // 先从 peek_buffer 读取
            if self.peek_pos < self.peek_buffer.len() {
                let data = &self.peek_buffer[self.peek_pos..];
                let status = inflate(
                    self.decomp_state.as_mut().unwrap(),
                    data,
                    &mut writer,
                    MZFlush::None,
                ).await?;

                self.peek_pos += status.bytes_consumed;

                match status.status {
                    Ok(crate::MZStatus::StreamEnd) => {
                        self.is_eof = true;
                        return Ok(true);
                    }
                    Ok(crate::MZStatus::Ok) => {
                        if status.bytes_written > 0 {
                            return Ok(true);
                        }
                    }
                    Ok(crate::MZStatus::NeedDict) => {
                        return Err(Error::Msg("Need dictionary not supported".to_string()));
                    }
                    Err(_) => {
                        return Err(Error::Msg("Decompression error".to_string()));
                    }
                }
                continue;
            }

            // 如果输入缓冲区空了，从 inner 读取更多数据
            if self.input_offset == self.input_end {
                self.input_offset = 0;
                self.input_end = self.inner.read(self.input_buffer.as_mut_slice()).await?;
                if self.input_end == 0 {
                    self.is_eof = true;
                    // 尝试完成解压
                    let status = inflate(
                        self.decomp_state.as_mut().unwrap(),
                        &[],
                        &mut writer,
                        MZFlush::Finish,
                    ).await?;
                    return match status.status {
                        Ok(_) => Ok(true),
                        Err(_) => Err(Error::Msg("Decompression error".to_string())),
                    };
                }
            }

            // 解压数据
            let status = inflate(
                self.decomp_state.as_mut().unwrap(),
                &self.input_buffer[self.input_offset..self.input_end],
                &mut writer,
                MZFlush::None,
            ).await?;

            self.input_offset += status.bytes_consumed;

            match status.status {
                Ok(crate::MZStatus::StreamEnd) => {
                    self.is_eof = true;
                    return Ok(true);
                }
                Ok(crate::MZStatus::Ok) => {
                    // 如果有输出数据，返回 true
                    if status.bytes_written > 0 {
                        return Ok(true);
                    }
                    // 否则继续循环读取更多输入
                }
                Ok(crate::MZStatus::NeedDict) => {
                    return Err(Error::Msg("Need dictionary not supported".to_string()));
                }
                Err(_) => {
                    return Err(Error::Msg("Decompression error".to_string()));
                }
            }
        }
    }
}

impl<R: Read + Send> Read for InflateReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> impl Future<Output = std::io::Result<usize>> + Send {
        async move {
            loop {
                match self.mode {
                    ReaderMode::Detecting => {
                        // 尝试检测
                        let is_compressed = self.try_detect().await
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

                        if is_compressed {
                            self.mode = ReaderMode::Decompressing;
                        } else {
                            self.mode = ReaderMode::PassThrough;
                        }
                        // 继续循环，现在已经确定了模式
                    }
                    ReaderMode::Decompressing => {
                        // 先从已解压缓冲区读取
                        let available = self.buffer.len() - self.buffer_pos;
                        if available > 0 {
                            let to_copy = available.min(buf.len());
                            buf[..to_copy].copy_from_slice(&self.buffer[self.buffer_pos..self.buffer_pos + to_copy]);
                            self.buffer_pos += to_copy;
                            return Ok(to_copy);
                        }

                        // 按需解压更多数据
                        let decompressed = self.decompress_more().await
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

                        if !decompressed {
                            return Ok(0);
                        }

                        // 再次尝试从缓冲区读取
                        let available = self.buffer.len() - self.buffer_pos;
                        let to_copy = available.min(buf.len());
                        if to_copy > 0 {
                            buf[..to_copy].copy_from_slice(&self.buffer[self.buffer_pos..self.buffer_pos + to_copy]);
                            self.buffer_pos += to_copy;
                        }
                        return Ok(to_copy);
                    }
                    ReaderMode::PassThrough => {
                        // 直通模式：先从 peek_buffer 读取（仅自动检测后有预读数据时）
                        if self.peek_pos < self.peek_buffer.len() {
                            let available = self.peek_buffer.len() - self.peek_pos;
                            let to_copy = available.min(buf.len());
                            buf[..to_copy].copy_from_slice(&self.peek_buffer[self.peek_pos..self.peek_pos + to_copy]);
                            self.peek_pos += to_copy;
                            // 如果读完了 peek_buffer，就清理掉释放内存
                            if self.peek_pos == self.peek_buffer.len() {
                                self.peek_buffer.clear();
                                self.peek_pos = 0;
                            }
                            return Ok(to_copy);
                        }

                        // 直接从 inner 读取，完全透传
                        return self.inner.read(buf).await;
                    }
                }
            }
        }
    }

    fn flush(&mut self) -> impl Future<Output = std::io::Result<()>> + Send {
        async move {
            match self.mode {
                ReaderMode::PassThrough => {
                    // 直通模式直接透传 flush
                    self.inner.flush().await
                }
                _ => {
                    Ok(())
                }
            }
        }
    }
}

impl<R: Seek + Read + Send> Seek for InflateReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> impl Future<Output = std::io::Result<u64>> + Send {
        async move {
            loop {
                match self.mode {
                    ReaderMode::Detecting => {
                        // 先检测
                        let is_compressed = self.try_detect().await
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
                        if is_compressed {
                            self.mode = ReaderMode::Decompressing;
                        } else {
                            self.mode = ReaderMode::PassThrough;
                        }
                        // 继续循环
                    }
                    ReaderMode::Decompressing => {
                        let new_pos = match pos {
                            SeekFrom::Start(offset) => offset as usize,
                            SeekFrom::End(offset) => (self.buffer.len() as i64 + offset) as usize,
                            SeekFrom::Current(offset) => (self.buffer_pos as i64 + offset) as usize,
                        };

                        // 如果目标位置超过已解压数据，尝试解压更多数据
                        while new_pos > self.buffer.len() {
                            let decompressed = self.decompress_more().await
                                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
                            if !decompressed {
                                break;
                            }
                        }

                        if new_pos > self.buffer.len() {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "Seek beyond decompressed data",
                            ));
                        }

                        self.buffer_pos = new_pos;
                        return Ok(new_pos as u64);
                    }
                    ReaderMode::PassThrough => {
                        // 直通模式直接透传给 inner
                        // 注意：如果有未读完的 peek_buffer，先丢弃，因为位置会被 inner 的 seek 重置
                        if !self.peek_buffer.is_empty() {
                            self.peek_buffer.clear();
                            self.peek_pos = 0;
                        }
                        return self.inner.seek(pos).await;
                    }
                }
            }
        }
    }
}

/// 辅助 struct：将解压数据写入 Vec<u8>
struct BufferWriter<'a> {
    buffer: &'a mut Vec<u8>,
}

impl<'a> BufferWriter<'a> {
    fn new(buffer: &'a mut Vec<u8>) -> Self {
        Self { buffer }
    }
}

impl<'a> Write for BufferWriter<'a> {
    fn write(&mut self, buf: &[u8]) -> impl Future<Output = std::io::Result<usize>> + Send {
        async move {
            self.buffer.extend_from_slice(buf);
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> impl Future<Output = std::io::Result<()>> + Send {
        async move {
            Ok(())
        }
    }
}

impl<'a> Seek for BufferWriter<'a> {
    fn seek(&mut self, _pos: SeekFrom) -> impl Future<Output = std::io::Result<u64>> + Send {
        async move {
            Ok(self.buffer.len() as u64)
        }
    }
}

/// 简单的 Vec 写入器，用于检测
struct VecWriter {
    buf: Vec<u8>,
}

impl Write for VecWriter {
    fn write(&mut self, buf: &[u8]) -> impl Future<Output = std::io::Result<usize>> + Send {
        async move {
            self.buf.extend_from_slice(buf);
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> impl Future<Output = std::io::Result<()>> + Send {
        async move {
            Ok(())
        }
    }
}

impl Seek for VecWriter {
    fn seek(&mut self, _pos: SeekFrom) -> impl Future<Output = std::io::Result<u64>> + Send {
        async move {
            Ok(self.buf.len() as u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deflate::compress_to_vec;
    use std::io::Cursor;

    #[tokio::test]
    async fn test_inflate_reader() {
        let test_data = b"Hello, world! This is a test of the InflateReader functionality.";
        let compressed = compress_to_vec(test_data, 6);

        let cursor = Cursor::new(compressed);
        let mut reader = InflateReader::new(cursor);

        // 测试 Read
        let mut output = Vec::new();
        let mut buffer = [0; 8];
        loop {
            let n = reader.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..n]);
        }
        assert_eq!(output, test_data);

        // 测试随机访问
        assert_eq!(reader.get(0), Some(&b'H'));
        assert_eq!(reader.get(test_data.len() - 1), Some(&b'.'));
        assert_eq!(reader.get(test_data.len()), None);

        // 测试 get_slice
        assert_eq!(reader.get_slice(0, 5), Some(&b"Hello"[..]));
        assert_eq!(reader.get_slice(7, 12), Some(&b"world"[..]));

        // 测试 reset_position
        reader.reset_position();
        let mut buffer2 = [0; 5];
        reader.read(&mut buffer2).await.unwrap();
        assert_eq!(&buffer2, b"Hello");
    }

    #[tokio::test]
    async fn test_seek() {
        let test_data = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let compressed = compress_to_vec(test_data, 6);

        let cursor = Cursor::new(compressed);
        let mut reader = InflateReader::new(cursor);

        // 先全部解压
        let mut output = Vec::new();
        let mut buffer = [0; 32];
        loop {
            let n = reader.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..n]);
        }

        // 测试 Seek
        reader.seek(SeekFrom::Start(10)).await.unwrap();
        let mut buffer2 = [0; 6];
        reader.read(&mut buffer2).await.unwrap();
        assert_eq!(&buffer2, b"ABCDEF");

        reader.seek(SeekFrom::Current(-4)).await.unwrap();
        reader.read(&mut buffer2[0..2]).await.unwrap();
        assert_eq!(&buffer2[0..2], b"CD");

        reader.seek(SeekFrom::End(-10)).await.unwrap();
        reader.read(&mut buffer2).await.unwrap();
        assert_eq!(&buffer2, b"QRSTUV");
    }

    #[tokio::test]
    async fn test_passthrough() {
        let test_data = b"This is not compressed data, it should pass through directly.";

        let cursor = Cursor::new(test_data);
        let mut reader = InflateReader::new(cursor);

        // 测试 Read
        let mut output = Vec::new();
        let mut buffer = [0; 8];
        loop {
            let n = reader.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..n]);
        }
        assert_eq!(output, test_data);
    }

    #[tokio::test]
    async fn test_force_decompress() {
        let test_data = b"Hello, forced decompress mode!";
        let compressed = compress_to_vec(test_data, 6);

        let cursor = Cursor::new(compressed);
        let mut reader = InflateReader::new_decompress(cursor);

        // 测试 Read
        let mut output = Vec::new();
        let mut buffer = [0; 8];
        loop {
            let n = reader.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..n]);
        }
        assert_eq!(output, test_data);
    }

    #[tokio::test]
    async fn test_force_passthrough() {
        let test_data = b"Hello, forced passthrough mode!";

        let cursor = Cursor::new(test_data);
        let mut reader = InflateReader::new_passthrough(cursor);

        // 测试 Read
        let mut output = Vec::new();
        let mut buffer = [0; 8];
        loop {
            let n = reader.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..n]);
        }
        assert_eq!(output, test_data);
    }
}
