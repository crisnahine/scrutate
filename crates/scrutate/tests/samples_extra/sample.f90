module physics
  implicit none
contains
  function kinetic_energy(mass, velocity) result(e)
    real, intent(in) :: mass, velocity
    real :: e
    e = 0.5 * mass * velocity**2
  end function kinetic_energy

  subroutine report(e)
    real, intent(in) :: e
    print *, "energy:", e
  end subroutine report
end module physics

program main
  use physics
  call report(kinetic_energy(2.0, 3.0))
end program main
