; Accept a bare SHA-256 or the first token of sha256sum output.
; Return an empty string for malformed input. Preserve caller registers.
Function ParsePackageChecksum
  Exch $R0
  Push $R1
  Push $R2
  Push $R3
  Push $R4
  StrCpy $R1 0
  StrCpy $R4 ""
  checksum_skip_space:
    StrCpy $R2 $R0 1 $R1
    StrCmp $R2 " " checksum_skip_next
    StrCmp $R2 "$\t" checksum_skip_next
    StrCmp $R2 "$\r" checksum_skip_next
    StrCmp $R2 "$\n" checksum_skip_next checksum_token
  checksum_skip_next:
    IntOp $R1 $R1 + 1
    Goto checksum_skip_space
  checksum_token:
    StrCpy $R2 $R0 1 $R1
    StrCmp $R2 "" checksum_validate
    StrCmp $R2 " " checksum_validate
    StrCmp $R2 "$\t" checksum_validate
    StrCmp $R2 "$\r" checksum_validate
    StrCmp $R2 "$\n" checksum_validate
    ${StrCase} $R2 $R2 "L"
    ${StrLoc} $R3 "0123456789abcdef" $R2 ">"
    StrCmp $R3 "" checksum_invalid
    StrCpy $R4 "$R4$R2"
    StrLen $R3 $R4
    IntCmp $R3 64 checksum_next checksum_next checksum_invalid
  checksum_next:
    IntOp $R1 $R1 + 1
    Goto checksum_token
  checksum_validate:
    StrLen $R3 $R4
    IntCmp $R3 64 checksum_done checksum_invalid checksum_invalid
  checksum_invalid:
    StrCpy $R4 ""
  checksum_done:
    StrCpy $R0 $R4
    Pop $R4
    Pop $R3
    Pop $R2
    Pop $R1
    Exch $R0
FunctionEnd
