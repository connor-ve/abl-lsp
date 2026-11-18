/* Uses .NET types through the bridge (Windows). The editor knows the common
   System.* types from abl's stubs. */
USING System.Text.StringBuilder FROM ASSEMBLY.
USING System.Collections.Generic.*.

DEFINE VARIABLE sb   AS StringBuilder NO-UNDO.
DEFINE VARIABLE lst  AS List<CHARACTER> NO-UNDO.
DEFINE VARIABLE cDir AS CHARACTER NO-UNDO.

sb = NEW StringBuilder().
sb:Append("Hello, "):Append(System.Environment:UserName).
lst = NEW List<CHARACTER>().
lst:Add(sb:ToString()).
cDir = System.IO.Path:Combine(System.IO.Path:GetTempPath(), "abl").
IF NOT System.IO.Directory:Exists(cDir) THEN
    System.IO.Directory:CreateDirectory(cDir).
MESSAGE lst:Count System.String:IsNullOrEmpty(cDir).
