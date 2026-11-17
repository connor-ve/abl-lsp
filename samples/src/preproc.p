/* preproc.p -- preprocessor names, &IF branches and include arguments */
&SCOPED-DEFINE VAR-TYPE INTEGER
&SCOPED-DEFINE LOOP-VAR iRow

{inc/ttdef.i ttOrder &key=OrderNum}
{inc/ttdef.i ttLine &key=LineNum}

DEFINE VARIABLE {&LOOP-VAR} AS {&VAR-TYPE} NO-UNDO.

&IF DEFINED(TTDEF-LOADED) &THEN
DEFINE VARIABLE cLoaded AS CHARACTER NO-UNDO.
&ELSE
DEFINE VARIABLE cMissing AS CHARACTER NO-UNDO.
&ENDIF

&IF "{&OPSYS}" = "WIN32" OR "{&OPSYS}" = "UNIX" &THEN
DEFINE VARIABLE cOs AS CHARACTER NO-UNDO.
&ENDIF

DO {&LOOP-VAR} = 1 TO 3:
    CREATE ttOrder.
    ttOrder.OrderNum = {&LOOP-VAR}.
    cLoaded = ttOrder.Descr.
END.
FOR EACH ttLine:
    DISPLAY ttLine.LineNum cOs.
END.
