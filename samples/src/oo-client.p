/* oo-client.p -- method chains, static access, CAST, NEW, overloads, built-in classes */
USING oo.*.
USING Progress.Json.ObjectModel.*.

DEFINE VARIABLE oSq    AS Square     NO-UNDO.
DEFINE VARIABLE oShape AS Shape      NO-UNDO.
DEFINE VARIABLE oJson  AS JsonObject NO-UNDO.
DEFINE VARIABLE cText  AS CHARACTER  NO-UNDO.

DO ON ERROR UNDO, THROW:
    oSq = Square:Make(2.0).
    cText = oSq:Grow(1.0):Describe("big ").
    cText = Square:Unit:ToString().
    oShape = oSq.
    cText = CAST(oShape, Square):Grow(2.0):ToString().
    cText = NEW Square(3.0):Describe().
    oJson = NEW JsonObject().
    oJson:Add("side", oSq:Side).
    oJson:Add("n", 5).
    oJson:Add("when", TODAY).
    oJson:Add("child", NEW JsonObject()).
    oJson:Add("big", oSq:Area() > 1).
    oJson:Add("label", SUBSTRING(cText, 1, 3)).
    cText = STRING(oJson:GetJsonText()).
    MESSAGE cText.

    CATCH eErr AS AppError:
        MESSAGE eErr:GetMessage(1) eErr:ReturnValue.
    END CATCH.
END.
