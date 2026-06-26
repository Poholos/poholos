/* nRF52833 (BBC micro:bit v2): 512 KiB flash, 128 KiB RAM.
   No SoftDevice reservation: the planned BLE stack (nrf-sdc) links into
   the application image. */
MEMORY
{
  FLASH : ORIGIN = 0x00000000, LENGTH = 512K
  RAM   : ORIGIN = 0x20000000, LENGTH = 128K
}
