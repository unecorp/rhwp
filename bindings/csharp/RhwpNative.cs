using System;
using System.Runtime.InteropServices;
using System.Text;

namespace Rhwp;

public static class RhwpNative
{
    public const int AllPages = -1;

    private const string NativeLibraryName = "rhwp_native_ffi";

    public static string ExportText(string inputPath, string outputDirectory, int page = AllPages)
    {
        IntPtr result = rhwp_export_text(ToUtf8NullTerminated(inputPath), ToUtf8NullTerminated(outputDirectory), page);
        return TakeResultString(result);
    }

    public static string ExportMarkdown(string inputPath, string outputDirectory, int page = AllPages)
    {
        IntPtr result = rhwp_export_markdown(ToUtf8NullTerminated(inputPath), ToUtf8NullTerminated(outputDirectory), page);
        return TakeResultString(result);
    }

    /// <summary>
    /// 파일로 내보내지 않고 페이지 텍스트를 JSON 문자열로 돌려준다.
    /// </summary>
    /// <remarks>
    /// [#3891] 이 바인딩은 <c>rhwp_read_text</c> 가 C ABI 에 추가된 뒤에도 반영되지
    /// 않아 Swift 에만 있던 기능이었다. C 표면 계약 가드가 그 표류를 검출해 채웠다.
    /// </remarks>
    public static string ReadText(string inputPath, int page = AllPages)
    {
        IntPtr result = rhwp_read_text(ToUtf8NullTerminated(inputPath), page);
        return TakeResultString(result);
    }

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr rhwp_export_text(byte[] inputPath, byte[] outputDirectory, int page);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr rhwp_export_markdown(byte[] inputPath, byte[] outputDirectory, int page);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr rhwp_read_text(byte[] inputPath, int page);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern void rhwp_string_free(IntPtr value);

    private static byte[] ToUtf8NullTerminated(string value)
    {
        if (value is null)
        {
            throw new ArgumentNullException(nameof(value));
        }

        byte[] utf8 = Encoding.UTF8.GetBytes(value);
        Array.Resize(ref utf8, utf8.Length + 1);
        return utf8;
    }

    private static string TakeResultString(IntPtr result)
    {
        if (result == IntPtr.Zero)
        {
            throw new InvalidOperationException("Native rhwp call returned a null result pointer.");
        }

        try
        {
            return Marshal.PtrToStringUTF8(result)
                ?? throw new InvalidOperationException("Native rhwp call returned invalid UTF-8.");
        }
        finally
        {
            rhwp_string_free(result);
        }
    }
}

/// <summary>
/// 열려 있는 문서 세션. 여러 번 편집한 뒤 한 번 저장하는 작업을 위한 표면이다.
/// </summary>
/// <remarks>
/// <c>rhwp_export_text</c> 계열은 호출마다 파일을 다시 파싱하고 결과를 파일로
/// 떨군다. 마크다운을 HWPX 로 조립하는 작업에는 맞지 않는다 — 편집이 호출 사이에
/// 남아야 하기 때문이다.
/// <para>
/// 핸들은 포인터가 아니라 정수다. 잘못된 값을 넘겨도 정의되지 않은 동작이 되지
/// 않고 오류로 응답한다. <c>0</c> 은 언제나 유효하지 않은 핸들이다.
/// </para>
/// </remarks>
public static class RhwpSession
{
    private const string NativeLibraryName = "rhwp_native_ffi";

    /// <summary>유효하지 않은 핸들.</summary>
    public const ulong InvalidHandle = 0;

    /// <summary>
    /// HWPX 파일을 열고 핸들을 돌려준다.
    /// </summary>
    /// <param name="inputPath">HWPX 파일 경로.</param>
    /// <returns>문서 핸들. 실패하면 <see cref="InvalidHandle"/> — 사유는 <see cref="LastError"/>.</returns>
    public static ulong Open(string inputPath) =>
        rhwp_document_open(ToUtf8(inputPath));

    /// <summary>
    /// 바이트에서 직접 연다. 업로드 스트림을 임시 파일로 떨구지 않기 위함이다.
    /// </summary>
    /// <param name="data">HWPX 바이트.</param>
    /// <returns>문서 핸들. 실패하면 <see cref="InvalidHandle"/>.</returns>
    public static ulong OpenBytes(ReadOnlySpan<byte> data)
    {
        unsafe
        {
            fixed (byte* pointer = data)
            {
                return rhwp_document_open_bytes(pointer, (nuint)data.Length);
            }
        }
    }

    /// <summary>핸들을 해제한다. 유효하지 않은 핸들·이중 해제는 무시된다.</summary>
    /// <param name="handle">문서 핸들.</param>
    public static void Close(ulong handle) => rhwp_document_close(handle);

    /// <summary>열려 있는 문서 수. 누수 점검용이다.</summary>
    /// <returns>세션에 남아 있는 문서 수.</returns>
    public static ulong OpenCount() => rhwp_document_open_count();

    /// <summary>
    /// 마지막 오류를 읽는다. 읽으면 비워진다.
    /// </summary>
    /// <returns><c>{"error":"…"}</c> 형태의 JSON.</returns>
    public static string LastError() => TakeResultString(rhwp_last_error());

    /// <summary>구역 수와 구역별 문단 수. 붙여넣을 위치를 정하는 데 쓴다.</summary>
    /// <param name="handle">문서 핸들.</param>
    /// <returns>결과 JSON.</returns>
    public static string Info(ulong handle) => TakeResultString(rhwp_document_info(handle));

    /// <summary>
    /// 누름틀의 이름과 문단 위치를 읽는다. 조립할 자리를 찾는 데 쓴다.
    /// </summary>
    /// <param name="handle">문서 핸들.</param>
    /// <returns><c>{"ok":true,"anchors":[{name,occurrence,section,paragraph,nested}]}</c>.</returns>
    /// <remarks>
    /// CLI <c>fields --json</c> 과 겹쳐 보이지만 목적이 다르고, 그래서 내용도 다르다.
    /// 저쪽은 사람이 문서를 들여다보는 용도라 안내문·현재값까지 싣는다. 이쪽은
    /// "이 수준의 문단이 몇 번인가"만 답한다 — 그 답이 <see cref="DuplicateParagraph"/>
    /// 의 인자가 된다.
    /// </remarks>
    public static string FieldAnchors(ulong handle) => TakeResultString(rhwp_document_field_anchors(handle));

    /// <summary>
    /// HTML 조각을 지정한 위치에 붙여넣는다.
    /// </summary>
    /// <param name="handle">문서 핸들.</param>
    /// <param name="section">구역 인덱스.</param>
    /// <param name="paragraph">문단 인덱스.</param>
    /// <param name="charOffset">문단 안 문자 위치.</param>
    /// <param name="html">붙여넣을 HTML.</param>
    /// <returns>결과 JSON.</returns>
    public static string PasteHtml(ulong handle, uint section, uint paragraph, uint charOffset, string html) =>
        TakeResultString(rhwp_document_paste_html(handle, section, paragraph, charOffset, ToUtf8(html)));

    /// <summary>
    /// HTML 조각을 문서 맨 끝에 붙여넣는다.
    /// </summary>
    /// <param name="handle">문서 핸들.</param>
    /// <param name="html">붙여넣을 HTML.</param>
    /// <returns>결과 JSON.</returns>
    public static string AppendHtml(ulong handle, string html) =>
        TakeResultString(rhwp_document_append_html(handle, ToUtf8(html)));

    /// <summary>
    /// 이름으로 누름틀 값을 채운다.
    /// </summary>
    /// <param name="handle">문서 핸들.</param>
    /// <param name="name">누름틀 이름.</param>
    /// <param name="occurrence">같은 이름이 여러 개일 때 몇 번째인지(0 부터).</param>
    /// <param name="value">채울 값.</param>
    /// <returns>결과 JSON.</returns>
    /// <remarks>
    /// CLI <c>edit fill-fields</c> 는 첫 칸만 채우지만 여기서는 몇 번째인지 고를 수
    /// 있다. 표 머리글처럼 같은 이름이 여러 칸에 걸린 서식에서 이 차이가 결정적이다.
    /// </remarks>
    public static string SetField(ulong handle, string name, uint occurrence, string value) =>
        TakeResultString(rhwp_document_set_field(handle, ToUtf8(name), occurrence, ToUtf8(value)));

    /// <summary>
    /// 문단을 서식·누름틀째 복제해 지정 위치에 넣는다.
    /// </summary>
    /// <param name="handle">문서 핸들.</param>
    /// <param name="section">구역 인덱스.</param>
    /// <param name="sourceParagraph">복제할 원본 문단.</param>
    /// <param name="destParagraph">복제본을 넣을 위치. 구역 문단 수와 같으면 맨 끝.</param>
    /// <param name="count">복제 벌 수.</param>
    /// <returns>결과 JSON.</returns>
    /// <remarks>
    /// 서식 문서로 보고서를 조립하는 경로의 핵심이다. 템플릿은 각 수준을 한 벌씩만
    /// 들고 있으므로, 마크다운의 항목이 다섯 개면 그 수준의 문단을 다섯 벌로 늘린 뒤
    /// <see cref="SetField"/> 로 하나씩 채운다. 복제본은 앞머리 글머리표와 글자
    /// 모양까지 원본 그대로라 서식의 단일 출처가 템플릿에 남는다.
    /// </remarks>
    public static string DuplicateParagraph(
        ulong handle, uint section, uint sourceParagraph, uint destParagraph, uint count) =>
        TakeResultString(rhwp_document_duplicate_paragraph(
            handle, section, sourceParagraph, destParagraph, count));

    /// <summary>
    /// 문단을 지운다.
    /// </summary>
    /// <param name="handle">문서 핸들.</param>
    /// <param name="section">구역 인덱스.</param>
    /// <param name="paragraph">지울 문단.</param>
    /// <returns>결과 JSON.</returns>
    /// <remarks>
    /// 조립이 끝난 뒤 템플릿의 견본 문단을 걷어내는 데 쓴다. 견본을 남겨 두면 안내문이
    /// 그대로 인쇄되므로, 늘린 뒤 원본을 지우는 것이 한 벌이다. 여러 개를 지울 때는
    /// <b>인덱스가 큰 것부터</b> 지워야 앞쪽 인덱스가 밀리지 않는다.
    /// </remarks>
    public static string DeleteParagraph(ulong handle, uint section, uint paragraph) =>
        TakeResultString(rhwp_document_delete_paragraph(handle, section, paragraph));

    /// <summary>세션 문서를 HWPX 로 저장한다.</summary>
    /// <param name="handle">문서 핸들.</param>
    /// <param name="outputPath">저장할 경로.</param>
    /// <returns>결과 JSON.</returns>
    public static string SaveHwpx(ulong handle, string outputPath) =>
        TakeResultString(rhwp_document_save_hwpx(handle, ToUtf8(outputPath)));

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern ulong rhwp_document_open(byte[] inputPath);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern unsafe ulong rhwp_document_open_bytes(byte* data, nuint length);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern void rhwp_document_close(ulong handle);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern ulong rhwp_document_open_count();

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr rhwp_last_error();

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr rhwp_document_info(ulong handle);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr rhwp_document_field_anchors(ulong handle);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr rhwp_document_paste_html(
        ulong handle, uint section, uint paragraph, uint charOffset, byte[] html);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr rhwp_document_append_html(ulong handle, byte[] html);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr rhwp_document_set_field(
        ulong handle, byte[] name, uint occurrence, byte[] value);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr rhwp_document_duplicate_paragraph(
        ulong handle, uint section, uint sourceParagraph, uint destParagraph, uint count);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr rhwp_document_delete_paragraph(ulong handle, uint section, uint paragraph);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr rhwp_document_save_hwpx(ulong handle, byte[] outputPath);

    [DllImport(NativeLibraryName, CallingConvention = CallingConvention.Cdecl)]
    private static extern void rhwp_string_free(IntPtr value);

    private static byte[] ToUtf8(string value)
    {
        if (value is null)
        {
            throw new ArgumentNullException(nameof(value));
        }

        byte[] utf8 = Encoding.UTF8.GetBytes(value);
        Array.Resize(ref utf8, utf8.Length + 1);
        return utf8;
    }

    private static string TakeResultString(IntPtr result)
    {
        if (result == IntPtr.Zero)
        {
            throw new InvalidOperationException("Native rhwp call returned a null result pointer.");
        }

        try
        {
            return Marshal.PtrToStringUTF8(result)
                ?? throw new InvalidOperationException("Native rhwp call returned invalid UTF-8.");
        }
        finally
        {
            rhwp_string_free(result);
        }
    }
}
