/* .NET type. Stub written by abl from the public .NET API: common members only, for navigation and completion. .NET types map to ABL ones: String to CHARACTER, Int32 to INTEGER, Boolean to LOGICAL, DateTime to DATETIME. */
CLASS System.Windows.Forms.Application INHERITS System.Object FINAL:
    /* The main form's event loop: WAIT-FOR System.Windows.Forms.Application:Run(oForm). */
    METHOD PUBLIC STATIC VOID Run(INPUT mainForm AS System.Windows.Forms.Form).
    METHOD PUBLIC STATIC VOID Exit().
    METHOD PUBLIC STATIC VOID DoEvents().
END CLASS.
