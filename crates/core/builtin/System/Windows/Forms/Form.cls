/* .NET type. Stub written by abl from the public .NET API: common members only, for navigation and completion. .NET types map to ABL ones: String to CHARACTER, Int32 to INTEGER, Boolean to LOGICAL, DateTime to DATETIME. */
/* An ABL form usually inherits Progress.Windows.Form, which inherits this. */
CLASS System.Windows.Forms.Form INHERITS System.Windows.Forms.Control:
    DEFINE PUBLIC PROPERTY AcceptButton AS System.Windows.Forms.Button NO-UNDO GET. SET.
    DEFINE PUBLIC PROPERTY CancelButton AS System.Windows.Forms.Button NO-UNDO GET. SET.
    DEFINE PUBLIC PROPERTY MaximizeBox AS LOGICAL NO-UNDO GET. SET.
    DEFINE PUBLIC PROPERTY MinimizeBox AS LOGICAL NO-UNDO GET. SET.

    DEFINE PUBLIC EVENT Load SIGNATURE VOID (INPUT sender AS System.Object, INPUT e AS System.EventArgs).

    CONSTRUCTOR PUBLIC Form():
    END CONSTRUCTOR.

    METHOD PUBLIC VOID Close().
    METHOD PUBLIC VOID Activate().
    /* Modal: WAIT-FOR oForm:ShowDialog(). Returns a System.Windows.Forms.DialogResult. */
    METHOD PUBLIC System.Object ShowDialog().
END CLASS.
