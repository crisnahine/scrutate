library ieee;
use ieee.std_logic_1164.all;

entity blinker is
  port (clk : in std_logic; led : out std_logic);
end entity blinker;

architecture rtl of blinker is
  signal state : std_logic := '0';
begin
  process (clk)
  begin
    if rising_edge(clk) then
      state <= not state;
    end if;
  end process;
  led <= state;
end architecture rtl;
