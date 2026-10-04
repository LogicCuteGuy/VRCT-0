<div align="center">
<picture>
    <source srcset="/docs/img/vrct_logo_white.png" media="(prefers-color-scheme: dark)">
    <img src="/docs/img/vrct_logo_black.png" alt="VRCT-0 — VRChat Chatbox Translator &amp; Transcription" width="50%">
</picture>

# VRCT-0

แปลภาษาและถอดเสียงสำหรับการสนทนาใน VRChat

[English](/docs/readmes/README.en.md) · [日本語](/docs/readmes/README.ja.md) · [한국어](/docs/readmes/README.ko.md) · [繁體中文](/docs/readmes/README.zh-Hant.md) · [简体中文](/docs/readmes/README.zh-Hans.md) · [ไทย](/docs/readmes/README.th.md)

[เอกสาร](/docs/README.md) · [ดาวน์โหลด](https://github.com/LogicCuteGuy/VRCT-0/releases)
</div>

## สิ่งที่เปลี่ยนใน fork นี้

VRCT-0 เป็น fork ของ [VRCT](https://github.com/misyaguziya/VRCT) โดยคงรูปแบบการแปลและถอดเสียงไว้ และปรับส่วนต่อไปนี้:

- **Backend Rust:** แทน Python sidecar และ runtime ที่เคยรวมในแอป รวมการตั้งค่า ผู้ให้บริการแปล โมเดล OSC และโอเวอร์เลย์ไว้ในระบบ native
- **ระบบเสียง Windows:** เลือก Host/Device แยกสำหรับไมโครโฟนและลำโพง รองรับแหล่งเสียง WASAPI, playback loopback, ASIO และแผงตั้งค่าไดรเวอร์
- **เครื่องมือ native:** จับภาพ จัดเตรียม dataset ทำ annotation ฝึกและส่งออกโมเดลตรวจจับ รวมถึงประเมินการถอดเสียง
- **แบรนด์:** ชื่อ VRCT-0 ไอคอนใหม่ และโลโก้สำหรับพื้นหลังสว่างและมืด
- **รูปลักษณ์:** ธีมมืด สว่าง และตามระบบที่บันทึกตัวเลือกได้ พร้อมปรับปรุงคำแปลหน้าจอ
- **บิลด์และแจกจ่าย:** ใช้ Rust xtask เตรียมทรัพยากรและตรวจแพ็กเกจ อัปเดตจาก Releases ของ fork นี้ และไม่แจกโมเดลตรวจจับกล่องแชตเดิมที่มีข้อจำกัดด้านสิทธิ์

ดู [native backend](../native_pipeline.md), [ระบบเสียง](../windows_audio.md) และ [native tools](../native_tools.md) สำหรับรายละเอียด

## ฟีเจอร์

- แปลข้อความและส่งไปกล่องแชต VRChat ผ่าน OSC
- ถอดเสียงไมโครโฟนและเสียงลำโพง
- อ่านข้อความกล่องแชตด้วย OCR และแสดงคำแปลในบันทึกหรือโอเวอร์เลย์ SteamVR
- ใช้โมเดลภายในเครื่องหรือบริการแปลที่รองรับ
- เลือกธีม **มืด / สว่าง / ตามระบบ** และบันทึกตัวเลือกบนเครื่อง

เลือกภาษาที่ต้องการในตัวติดตั้งหรือหน้า **การตั้งค่า → รูปลักษณ์ → ภาษาหน้าจอ** และเปลี่ยนธีมได้ในหน้าเดียวกัน

ดาวน์โหลดจาก Releases ของรุ่นนี้และแตกไฟล์ทั้งหมดก่อนเปิด `VRCT.exe` ดู [วิธีบิลด์](../readme_build.md) หากต้องการสร้างแอปเอง

## เครดิตและลิขสิทธิ์

ดูแลและปรับแบรนด์โดย **LogicCuteGuy** พัฒนาต่อจาก [VRCT โดย m's software](https://github.com/misyaguziya/VRCT) โดยคงเครดิตผู้ร่วมพัฒนาและนักแปลเดิมไว้ ดู [MIT License](../../LICENSE) และ [NOTICE](../../NOTICE.md) สำหรับเงื่อนไของค์ประกอบภายนอก
