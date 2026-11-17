/* ttdef.i -- a temp-table named by the caller:  {inc/ttdef.i <name> &key=<field>} */
&IF DEFINED(TTDEF-NO-DESCR) = 0 &THEN
DEFINE TEMP-TABLE {1} NO-UNDO
    FIELD {&key} AS INTEGER
    FIELD Descr  AS CHARACTER
    INDEX idxKey IS PRIMARY UNIQUE {&key}.
&ENDIF
&GLOBAL-DEFINE TTDEF-LOADED yes
