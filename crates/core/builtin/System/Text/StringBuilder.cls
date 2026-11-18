/* .NET type. Stub written by abl from the public .NET API: common members only, for navigation and completion. .NET types map to ABL ones: String to CHARACTER, Int32 to INTEGER, Boolean to LOGICAL, DateTime to DATETIME. */
CLASS System.Text.StringBuilder INHERITS System.Object FINAL:
    DEFINE PUBLIC PROPERTY Length AS INTEGER NO-UNDO GET. SET.
    DEFINE PUBLIC PROPERTY Capacity AS INTEGER NO-UNDO GET. SET.

    CONSTRUCTOR PUBLIC StringBuilder():
    END CONSTRUCTOR.
    CONSTRUCTOR PUBLIC StringBuilder(INPUT capacity AS INTEGER):
    END CONSTRUCTOR.
    CONSTRUCTOR PUBLIC StringBuilder(INPUT value AS CHARACTER):
    END CONSTRUCTOR.

    /* Returns the builder, so calls chain: sb:Append("a"):Append(1). */
    METHOD PUBLIC System.Text.StringBuilder Append(INPUT value AS CHARACTER).
    METHOD PUBLIC System.Text.StringBuilder Append(INPUT value AS INTEGER).
    METHOD PUBLIC System.Text.StringBuilder Append(INPUT value AS INT64).
    METHOD PUBLIC System.Text.StringBuilder Append(INPUT value AS DECIMAL).
    METHOD PUBLIC System.Text.StringBuilder Append(INPUT value AS LOGICAL).
    METHOD PUBLIC System.Text.StringBuilder Append(INPUT value AS System.Object).
    METHOD PUBLIC System.Text.StringBuilder AppendLine().
    METHOD PUBLIC System.Text.StringBuilder AppendLine(INPUT value AS CHARACTER).
    METHOD PUBLIC System.Text.StringBuilder AppendFormat(INPUT fmt AS CHARACTER, INPUT arg0 AS System.Object).
    METHOD PUBLIC System.Text.StringBuilder Insert(INPUT index AS INTEGER, INPUT value AS CHARACTER).
    METHOD PUBLIC System.Text.StringBuilder Remove(INPUT startIndex AS INTEGER, INPUT length AS INTEGER).
    METHOD PUBLIC System.Text.StringBuilder Replace(INPUT oldValue AS CHARACTER, INPUT newValue AS CHARACTER).
    METHOD PUBLIC System.Text.StringBuilder Clear().
    METHOD PUBLIC OVERRIDE CHARACTER ToString().
END CLASS.
